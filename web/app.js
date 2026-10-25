(() => {
  'use strict';

  const $ = (selector, root = document) => root.querySelector(selector);
  const $$ = (selector, root = document) => Array.from(root.querySelectorAll(selector));
  const escapeHTML = (value) => String(value ?? '').replace(/[&<>"']/g, (character) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[character]));
  const icon = (name, className = '') => `<svg class="icon ${className}" aria-hidden="true"><use href="#i-${name}"/></svg>`;
  const selectedProducts = new Set();
  const selectedStores = new Set();
  let catalog = { products: [], stores: [] };
  let state = null;
  let category = 'iphone';
  let stopped = false;
  let polling = false;
  let refreshWasActive = false;
  let catalogLoaded = false;
  let catalogLoading = false;
  let monitorBusy = false;
  let lastRowsSignature = '';
  let lastNotificationsSignature = '';
  let toastTimer;
  let pollTimer;
  let token = '';

  // The launch token is deliberately kept out of the URL before any navigation.
  // sessionStorage lets a reload reconnect without persisting it between browser sessions.
  const fragment = new URLSearchParams(location.hash.slice(1));
  try {
    token = fragment.get('token') || sessionStorage.getItem('pickup-monitor-token') || '';
    if (fragment.has('token')) sessionStorage.setItem('pickup-monitor-token', token);
  } catch (_) {
    token = fragment.get('token') || '';
  }
  if (fragment.has('token')) history.replaceState(null, '', `${location.pathname}${location.search}`);
  window.addEventListener('hashchange', () => {
    // A reopened app can give an existing tab a fresh session after a restart.
    if (new URLSearchParams(location.hash.slice(1)).has('token')) location.reload();
  });

  function showMessage(element, message, tone = 'warning') {
    element.textContent = message;
    element.classList.remove('warning', 'error', 'success');
    if (tone) element.classList.add(tone);
    element.hidden = !message;
  }

  function toast(message, failed = false) {
    clearTimeout(toastTimer);
    const element = $('#toast');
    element.textContent = message;
    element.classList.toggle('error', failed);
    element.hidden = false;
    toastTimer = setTimeout(() => { element.hidden = true; }, failed ? 7000 : 4000);
  }

  async function api(path, options = {}) {
    if (!token) throw new Error('Open the dashboard from Apple Pickup Monitor to connect securely.');
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 20000);
    try {
      const response = await fetch(path, {
        ...options,
        signal: controller.signal,
        headers: { 'X-Session-Token': token, ...(options.body ? { 'Content-Type': 'application/json' } : {}), ...options.headers },
        cache: 'no-store',
        credentials: 'same-origin',
      });
      const contentType = response.headers.get('content-type') || '';
      const result = contentType.includes('application/json') ? await response.json() : null;
      if (!response.ok) {
        if (response.status === 401 || response.status === 403) {
          stopped = true;
          clearTimeout(pollTimer);
          showMessage($('#connection-banner'), 'This dashboard session has expired. Reopen it from Apple Pickup Monitor to reconnect.');
          updateConnectionStatus(false);
        }
        throw new Error(result?.error || `The request could not be completed (${response.status}).`);
      }
      return result || {};
    } catch (error) {
      if (error.name === 'AbortError') throw new Error('The service took too long to respond. Please try again.');
      if (error instanceof TypeError) throw new Error('Unable to reach Pickup Monitor. Check that the app is still running.');
      throw error;
    } finally {
      clearTimeout(timeout);
    }
  }

  const send = (path, body = {}, method = 'POST') => api(path, { method, body: JSON.stringify(body) });

  function timeLabel(timestamp) {
    if (!timestamp) return 'Not checked yet';
    const date = new Date(Number(timestamp));
    if (Number.isNaN(date.getTime())) return 'Not checked yet';
    const today = new Date();
    return date.toLocaleString('en-AU', {
      ...(date.toDateString() === today.toDateString() ? {} : { day: 'numeric', month: 'short' }),
      hour: 'numeric', minute: '2-digit',
    });
  }

  function relativeTime(timestamp) {
    const milliseconds = Number(timestamp);
    if (!milliseconds || !Number.isFinite(milliseconds)) return 'Not checked yet';
    const seconds = Math.max(0, Math.floor((Date.now() - milliseconds) / 1000));
    if (seconds < 10) return 'Just now';
    if (seconds < 60) return `${seconds}s ago`;
    if (seconds < 3600) return `${Math.floor(seconds / 60)}m ago`;
    return timeLabel(milliseconds);
  }

  function unknownReason(availability) {
    const reason = availability?.reason;
    if (typeof reason === 'string') return reason;
    return reason?.kind || reason?.type || '';
  }

  function availabilityLabel(availability = {}) {
    if (availability.kind === 'in_stock') return { label: 'Available', tone: 'positive', detail: 'Ready for pickup' };
    if (availability.kind === 'out_of_stock') return { label: 'Unavailable', tone: 'neutral', detail: '' };
    const reason = unknownReason(availability);
    const labels = {
      not_yet_checked: ['Waiting', 'The first check hasn’t completed'],
      pickup_not_open: ['Pickup not open', 'Apple hasn’t opened pickup yet'],
      blocked: ['Unknown', 'Apple temporarily blocked the check'],
      rate_limited: ['Unknown', 'Waiting for Apple’s rate limit'],
      transport: ['Unknown', 'Connection failed; will retry'],
      schema_drift: ['Unknown', 'Apple’s response was not recognised'],
      apple_error: ['Unknown', 'Apple could not confirm availability'],
    };
    const [label, detail] = labels[reason] || ['Unknown', 'Availability could not be confirmed'];
    return { label, detail, tone: reason === 'not_yet_checked' ? 'neutral' : 'warning' };
  }

  function updateConnectionStatus(connected) {
    const element = $('#monitor-status');
    if (!connected) {
      element.className = 'status-pill warning';
      element.innerHTML = '<span class="status-dot"></span>Disconnected';
      $('#monitor-toggle').disabled = true;
      return;
    }
    element.className = `status-pill ${state?.running ? 'positive' : 'neutral'}`;
    element.innerHTML = `<span class="status-dot"></span>${state?.running ? 'Monitoring' : 'Paused'}`;
  }

  function renderClock() {
    if (!state) return;
    const next = $('#next-check');
    const detail = $('#next-check-detail');
    const label = next.closest('.summary-card')?.querySelector('.summary-label');
    const progress = state.running ? state.progress : null;
    if (label) label.textContent = progress && !stopped ? 'Current round' : 'Next check';
    next.style.fontSize = '';
    if (stopped) {
      next.textContent = '—';
      detail.textContent = 'Service disconnected';
    } else if (!state.running) {
      next.textContent = 'Paused';
      next.style.fontSize = '24px';
      detail.textContent = state.targets?.length ? 'Start when you’re ready' : 'Add a monitoring target to get started';
    } else if (progress) {
      const total = Math.max(0, Number(progress.totalStores) || 0);
      const completed = Math.min(total, Math.max(0, Number(progress.completedStores) || 0));
      next.style.fontSize = '22px';
      next.textContent = total ? `${completed} / ${total} store${total === 1 ? '' : 's'}` : 'Preparing…';
      if (progress.phase === 'waiting') {
        detail.textContent = 'Waiting for the next request slot. Completed results are shown below.';
      } else if (progress.activeLocation) {
        detail.textContent = `Checking ${progress.activeLocation}. Requests are paced to respect Apple’s limits.`;
      } else {
        detail.textContent = 'Checking your stores. Results appear as each check completes.';
      }
    } else {
      const seconds = state.nextCheckAt ? Math.max(0, Math.ceil((Number(state.nextCheckAt) - Date.now()) / 1000)) : 0;
      next.style.fontSize = seconds ? '' : '22px';
      next.textContent = seconds ? (seconds >= 60 ? `${Math.floor(seconds / 60)}m ${String(seconds % 60).padStart(2, '0')}s` : `${seconds}s`) : 'Checking…';
      detail.textContent = state.paced ? 'Next round is paced to respect Apple’s request limits' : `Minimum interval: ${state.intervalSeconds || 60} seconds`;
    }
    $$('[data-checked-at]').forEach((element) => { element.textContent = relativeTime(element.dataset.checkedAt); });
  }

  function renderState() {
    if (!state) return;
    const targets = state.targets || [];
    const available = targets.filter((entry) => entry.availability?.kind === 'in_stock').length;
    const unknown = targets.filter((entry) => !['in_stock', 'out_of_stock'].includes(entry.availability?.kind)).length;
    const storeCount = new Set(targets.map((entry) => entry.target?.storeNumber)).size;
    $('#watch-count').textContent = targets.length;
    $('#nav-count').textContent = targets.length;
    $('#watch-detail').textContent = targets.length ? `Across ${storeCount} Apple Store${storeCount === 1 ? '' : 's'}` : 'Products and stores you’re following';
    $('#available-count').textContent = available;
    $('#available-detail').textContent = targets.length ? (available ? `${available} match${available === 1 ? '' : 'es'} on your monitoring list` : unknown ? `${unknown} target${unknown === 1 ? '' : 's'} awaiting confirmation` : 'We’ll let you know when that changes') : 'Waiting for your first check';
    $('#app-version').textContent = state.version ? `v${state.version}` : 'Prototype';
    updateConnectionStatus(true);
    const button = $('#monitor-toggle');
    button.disabled = (!targets.length && !state.running) || monitorBusy;
    button.innerHTML = `${icon(state.running ? 'pause' : 'play')}<span>${state.running ? 'Pause monitoring' : 'Start monitoring'}</span>`;
    button.setAttribute('aria-pressed', String(Boolean(state.running)));
    $('#watch-empty').hidden = targets.length > 0;
    $('#watch-table-wrap').hidden = !targets.length;
    showMessage($('#health-banner'), state.notice || (state.healthy === false && state.running ? 'Some checks could not confirm availability. Unknown results are not the same as unavailable stock.' : ''));
    const rows = targets.map((entry) => {
      const target = entry.target || {};
      const availability = availabilityLabel(entry.availability);
      if (unknownReason(entry.availability) === 'not_yet_checked' && state.running) {
        if (entry.checkPhase === 'checking') availability.label = 'Checking';
        else if (entry.checkPhase === 'queued') availability.label = 'Queued';
      }
      if (entry.availabilityDetail) availability.detail = entry.availabilityDetail;
      const checkedAt = entry.lastCheckedMs || '';
      return `<tr><td><div class="product-cell"><span class="product-symbol">${icon('bag')}</span><div><span class="product-name">${escapeHTML(target.productName || target.partNumber)}</span><span class="product-part">${escapeHTML(target.partNumber)}</span></div></div></td><td><span class="store-name">${escapeHTML(target.storeTitle || target.storeNumber)}</span><span class="store-number">${escapeHTML(target.storeNumber)}</span></td><td><span class="status-pill ${availability.tone}"><span class="status-dot"></span>${availability.label}</span>${availability.detail ? `<span class="availability-detail">${escapeHTML(availability.detail)}</span>` : ''}</td><td><span class="checked-time" data-checked-at="${escapeHTML(checkedAt)}" title="${escapeHTML(checkedAt ? new Date(Number(checkedAt)).toLocaleString('en-AU') : '')}">${relativeTime(checkedAt)}</span></td><td><button class="icon-button remove" type="button" data-remove="${escapeHTML(entry.key || `${target.locale}|${target.storeNumber}|${target.partNumber}`)}" aria-label="Remove ${escapeHTML(target.productName || target.partNumber)} at ${escapeHTML(target.storeTitle || target.storeNumber)}">${icon('trash')}</button></td></tr>`;
    }).join('');
    // Keep row controls stable between polls; the clock updates their ages separately.
    const rowsSignature = JSON.stringify(targets);
    if (rowsSignature !== lastRowsSignature) {
      const focusedKey = document.activeElement?.dataset?.remove;
      $('#watch-rows').innerHTML = rows;
      lastRowsSignature = rowsSignature;
      if (focusedKey) $$('[data-remove]').find((element) => element.dataset.remove === focusedKey)?.focus({ preventScroll: true });
    }
    renderNotifications();
    renderClock();
    const refreshButton = $('#refresh-catalog');
    refreshButton.disabled = Boolean(state.catalogRefreshing);
    refreshButton.innerHTML = `${icon('refresh')}${state.catalogRefreshing ? 'Refreshing…' : 'Refresh catalog'}`;
    if (refreshWasActive && !state.catalogRefreshing) loadCatalog(true).catch((error) => toast(error.message, true));
    refreshWasActive = Boolean(state.catalogRefreshing);
  }

  function renderNotifications() {
    const notifications = state.notifications || [];
    $('#notification-empty').hidden = notifications.length > 0;
    $('#notification-list').hidden = !notifications.length;
    const settings = state.settings || {};
    $('#notification-empty-text').textContent = settings.pushoverEnabled || settings.barkEnabled ? 'Pickup alerts and test messages will appear here.' : 'Connect Pushover or Bark in Settings to get pickup alerts.';
    const signature = JSON.stringify(notifications);
    if (signature === lastNotificationsSignature) return;
    lastNotificationsSignature = signature;
    $('#notification-list').innerHTML = notifications.slice(0, 20).map((item) => {
      const failed = ['failed', 'error'].includes(item.status);
      const sent = ['sent', 'delivered', 'success'].includes(item.status);
      const label = failed ? 'Failed' : sent ? 'Sent' : item.status ? String(item.status).replace(/_/g, ' ') : 'Pending';
      const channel = item.channel === 'pushover' ? 'Pushover' : item.channel === 'bark' ? 'Bark' : item.channel || 'Notification';
      return `<li class="notification-item"><span class="notification-icon ${failed ? 'failed' : ''}">${icon(failed ? 'info' : 'bell')}</span><div class="notification-copy"><div class="notification-title">${escapeHTML(item.title || 'Pickup update')}</div><p class="notification-message">${escapeHTML(item.message || '')}</p>${item.error ? `<p class="notification-message">${escapeHTML(item.error)}</p>` : ''}<div class="notification-meta"><span>${escapeHTML(channel)}</span><span>·</span><span class="status-pill ${failed ? 'negative' : sent ? 'positive' : 'neutral'}">${escapeHTML(label)}</span></div></div><time class="notification-time">${escapeHTML(timeLabel(item.createdAt))}</time></li>`;
    }).join('');
  }

  async function poll() {
    if (stopped || polling) return;
    polling = true;
    try {
      state = await api('/api/state');
      showMessage($('#connection-banner'), '');
      renderState();
    } catch (error) {
      showMessage($('#connection-banner'), error.message);
      updateConnectionStatus(false);
    } finally {
      polling = false;
      clearTimeout(pollTimer);
      if (!stopped) pollTimer = setTimeout(poll, 3000);
    }
  }

  async function loadCatalog(force = false) {
    if ((catalogLoaded && !force) || catalogLoading) return;
    catalogLoading = true;
    try {
      const result = await api('/api/catalog');
      catalog = { products: result.products || [], stores: result.stores || [] };
      catalogLoaded = true;
      renderProducts();
      renderStores();
    } finally { catalogLoading = false; }
  }

  function renderProducts() {
    const search = $('#product-search').value.trim().toLocaleLowerCase();
    const words = search.split(/\s+/).filter(Boolean);
    const products = catalog.products.filter((product) => product.category === category && words.every((word) => `${product.title} ${product.partNumber} ${product.family} ${product.capacity} ${product.color}`.toLocaleLowerCase().includes(word)));
    const visible = products.slice(0, 100);
    $('#product-options').innerHTML = visible.length ? visible.map((product) => `<label class="option-item ${selectedProducts.has(product.partNumber) ? 'selected' : ''}"><input type="checkbox" name="product" value="${escapeHTML(product.partNumber)}" ${selectedProducts.has(product.partNumber) ? 'checked' : ''}><span class="option-copy"><span class="option-title">${escapeHTML(product.title)}</span><span class="option-subtitle">${escapeHTML(product.partNumber)}</span></span></label>`).join('') : '<p class="list-message">No products match your search.<br>Try another model, capacity or colour.</p>';
    $('#product-match-count').textContent = products.length > visible.length ? `${visible.length} of ${products.length} · search to narrow` : `${products.length} product${products.length === 1 ? '' : 's'}`;
    renderSelections();
  }

  function renderStores() {
    const search = $('#store-search').value.trim().toLocaleLowerCase();
    const stores = catalog.stores.filter((store) => `${store.title} ${store.name} ${store.number}`.toLocaleLowerCase().includes(search));
    $('#store-options').innerHTML = stores.length ? stores.map((store) => `<label class="option-item ${selectedStores.has(store.number) ? 'selected' : ''}"><input type="checkbox" name="store" value="${escapeHTML(store.number)}" ${selectedStores.has(store.number) ? 'checked' : ''}><span class="option-copy"><span class="option-title">${escapeHTML(store.title || store.name)}</span><span class="option-subtitle">Apple Store · ${escapeHTML(store.number)}</span></span></label>`).join('') : '<p class="list-message">No stores match your search.<br>Try a city or a store name.</p>';
    $('#store-match-count').textContent = `${stores.length} store${stores.length === 1 ? '' : 's'}`;
    renderSelections();
  }

  function renderSelections() {
    const products = selectedProducts.size;
    const stores = selectedStores.size;
    const count = products * stores;
    $('#product-selected').textContent = `${products} selected`;
    $('#store-selected').textContent = `${stores} selected`;
    $('#selection-summary').textContent = count ? `${products} product${products === 1 ? '' : 's'} × ${stores} store${stores === 1 ? '' : 's'} = ${count} monitoring target${count === 1 ? '' : 's'}` : 'Select at least one product and one store.';
    $('#add-submit').disabled = !count;
    $('#add-submit').innerHTML = `${icon('plus')}Add ${count || ''} monitoring target${count === 1 ? '' : 's'}`;
  }

  async function openAdd() {
    showMessage($('#add-error'), '');
    $('#add-dialog').showModal();
    try { await loadCatalog(); }
    catch (error) { showMessage($('#add-error'), error.message); }
  }

  function openSettings() {
    const settings = state?.settings || {};
    $('#interval-input').value = state?.intervalSeconds || 60;
    $('#pushover-enabled').checked = Boolean(settings.pushoverEnabled);
    $('#bark-enabled').checked = Boolean(settings.barkEnabled);
    $('#pushover-configured').textContent = settings.pushoverConfigured ? 'Saved' : '';
    $('#bark-configured').textContent = settings.barkConfigured ? 'Saved' : '';
    ['pushover-token', 'pushover-user', 'bark-url'].forEach((id) => { $(`#${id}`).value = ''; });
    $('#pushover-token').placeholder = settings.pushoverConfigured ? 'Saved · leave blank to keep' : 'Your application token';
    $('#pushover-user').placeholder = settings.pushoverConfigured ? 'Saved · leave blank to keep' : 'Your Pushover user key';
    $('#bark-url').placeholder = settings.barkConfigured ? 'Saved · leave blank to keep' : 'https://api.day.app/your-key';
    showMessage($('#settings-message'), '');
    $('#settings-dialog').showModal();
  }

  $$('[data-open-add]').forEach((button) => button.addEventListener('click', openAdd));
  $$('[data-open-settings]').forEach((button) => button.addEventListener('click', openSettings));
  $$('[data-close-dialog]').forEach((button) => button.addEventListener('click', () => button.closest('dialog').close()));
  $$('.dialog').forEach((dialog) => dialog.addEventListener('click', (event) => {
    if (event.target !== dialog) return;
    const rect = dialog.getBoundingClientRect();
    if (event.clientX < rect.left || event.clientX > rect.right || event.clientY < rect.top || event.clientY > rect.bottom) dialog.close();
  }));

  $$('.category-tab').forEach((button) => button.addEventListener('click', () => {
    category = button.dataset.category;
    $$('.category-tab').forEach((tab) => {
      tab.classList.toggle('selected', tab === button);
      tab.setAttribute('aria-pressed', String(tab === button));
    });
    $('#product-search').value = '';
    renderProducts();
  }));
  $('#product-search').addEventListener('input', renderProducts);
  $('#store-search').addEventListener('input', renderStores);
  $('#product-options').addEventListener('change', (event) => {
    const input = event.target;
    if (input.name !== 'product') return;
    input.checked ? selectedProducts.add(input.value) : selectedProducts.delete(input.value);
    input.closest('.option-item').classList.toggle('selected', input.checked);
    renderSelections();
  });
  $('#store-options').addEventListener('change', (event) => {
    const input = event.target;
    if (input.name !== 'store') return;
    input.checked ? selectedStores.add(input.value) : selectedStores.delete(input.value);
    input.closest('.option-item').classList.toggle('selected', input.checked);
    renderSelections();
  });
  $('#clear-selection').addEventListener('click', () => {
    selectedProducts.clear(); selectedStores.clear(); renderProducts(); renderStores();
  });

  $('#add-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    if (!selectedProducts.size || !selectedStores.size) return;
    const button = $('#add-submit');
    button.disabled = true;
    button.setAttribute('aria-busy', 'true');
    showMessage($('#add-error'), '');
    try {
      await send('/api/targets', { storeNumbers: Array.from(selectedStores), partNumbers: Array.from(selectedProducts) });
      $('#add-dialog').close();
      selectedProducts.clear(); selectedStores.clear();
      renderProducts(); renderStores();
      toast('Your monitoring list has been updated.');
      await poll();
    } catch (error) { showMessage($('#add-error'), error.message); }
    finally { button.removeAttribute('aria-busy'); renderSelections(); }
  });

  $('#watch-rows').addEventListener('click', async (event) => {
    const button = event.target.closest('[data-remove]');
    if (!button || button.disabled) return;
    button.disabled = true;
    try {
      await send('/api/targets/remove', { key: button.dataset.remove });
      button.blur();
      await poll();
      toast('Monitoring target removed.');
    } catch (error) { toast(error.message, true); }
    finally { button.disabled = false; }
  });

  $('#monitor-toggle').addEventListener('click', async () => {
    if (!state || monitorBusy) return;
    const running = !state.running;
    monitorBusy = true;
    $('#monitor-toggle').disabled = true;
    $('#monitor-toggle').setAttribute('aria-busy', 'true');
    try { await send('/api/monitor', { running }); await poll(); }
    catch (error) { toast(error.message, true); }
    finally {
      monitorBusy = false;
      $('#monitor-toggle').removeAttribute('aria-busy');
      if (state && !stopped) renderState();
    }
  });

  $('#refresh-catalog').addEventListener('click', async () => {
    const button = $('#refresh-catalog');
    button.disabled = true;
    try {
      await send('/api/catalog/refresh', { category });
      refreshWasActive = true;
      toast('Refreshing products from Apple. This may take a moment.');
      await poll();
    } catch (error) { toast(error.message, true); button.disabled = false; }
  });

  $('#settings-form').addEventListener('submit', async (event) => {
    event.preventDefault();
    const button = $('#settings-submit');
    const message = $('#settings-message');
    const credentials = {};
    const pushoverToken = $('#pushover-token').value.trim();
    const pushoverUser = $('#pushover-user').value.trim();
    const barkUrl = $('#bark-url').value.trim();
    if (pushoverToken) credentials.pushoverToken = pushoverToken;
    if (pushoverUser) credentials.pushoverUser = pushoverUser;
    if (barkUrl) credentials.barkUrl = barkUrl;
    const settings = { intervalSeconds: Number($('#interval-input').value), pushoverEnabled: $('#pushover-enabled').checked, barkEnabled: $('#bark-enabled').checked };
    if (!Number.isInteger(settings.intervalSeconds) || settings.intervalSeconds < 60 || settings.intervalSeconds > 3600) {
      showMessage(message, 'Choose an interval between 60 and 3,600 seconds.'); return;
    }
    if (settings.pushoverEnabled && !state?.settings?.pushoverConfigured && (!pushoverToken || !pushoverUser)) {
      showMessage(message, 'Enter both your Pushover application token and user key to enable Pushover.'); return;
    }
    if (settings.barkEnabled && !state?.settings?.barkConfigured && !barkUrl) {
      showMessage(message, 'Enter your Bark push URL to enable Bark.'); return;
    }
    button.disabled = true;
    button.setAttribute('aria-busy', 'true');
    showMessage(message, '');
    try {
      if (Object.keys(credentials).length) await send('/api/credentials', credentials, 'PUT');
      await send('/api/settings', settings, 'PUT');
      ['pushover-token', 'pushover-user', 'bark-url'].forEach((id) => { $(`#${id}`).value = ''; });
      await poll();
      $('#pushover-configured').textContent = state?.settings?.pushoverConfigured ? 'Saved' : '';
      $('#bark-configured').textContent = state?.settings?.barkConfigured ? 'Saved' : '';
      showMessage(message, 'Settings saved. You can send a test notification below.', 'success');
      toast('Settings saved.');
    } catch (error) { showMessage(message, error.message); }
    finally { button.disabled = false; button.removeAttribute('aria-busy'); }
  });

  $$('[data-test-channel]').forEach((button) => button.addEventListener('click', async () => {
    const channel = button.dataset.testChannel;
    const name = channel === 'pushover' ? 'Pushover' : 'Bark';
    if ($('#pushover-token').value || $('#pushover-user').value || $('#bark-url').value) {
      showMessage($('#settings-message'), 'Save your updated credentials before sending a test.'); return;
    }
    button.disabled = true;
    button.setAttribute('aria-busy', 'true');
    button.textContent = 'Sending…';
    try {
      await send('/api/notifications/test', { channel });
      showMessage($('#settings-message'), `${name} test requested. Check your device and notification history for delivery status.`, 'success');
      await poll();
    } catch (error) { showMessage($('#settings-message'), error.message); }
    finally { button.disabled = false; button.removeAttribute('aria-busy'); button.textContent = 'Send test'; }
  }));

  $('#quit-button').addEventListener('click', () => $('#quit-dialog').showModal());
  $('#confirm-quit').addEventListener('click', async () => {
    const button = $('#confirm-quit');
    button.disabled = true;
    try {
      await send('/api/quit');
      stopped = true;
      clearTimeout(pollTimer);
      $('#quit-dialog').close();
      try { sessionStorage.removeItem('pickup-monitor-token'); } catch (_) { /* Storage may be disabled. */ }
      showMessage($('#connection-banner'), 'Pickup Monitor has quit. You can close this tab. Launch the app again whenever you’re ready.', 'success');
      updateConnectionStatus(false);
      $('#monitor-status').className = 'status-pill neutral';
      $('#monitor-status').innerHTML = '<span class="status-dot"></span>Stopped';
      renderClock();
    } catch (error) { toast(error.message, true); }
    finally { button.disabled = false; }
  });

  document.addEventListener('visibilitychange', () => { if (!document.hidden && !stopped) poll(); });
  setInterval(renderClock, 1000);
  if (token) poll();
  else {
    stopped = true;
    showMessage($('#connection-banner'), 'Open this dashboard from Apple Pickup Monitor to connect securely.');
    updateConnectionStatus(false);
  }
})();
