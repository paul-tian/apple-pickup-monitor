//! Observe individual Apple queries without changing their transport or rate limits.
//!
//! The upstream watcher publishes its snapshot only at the end of a complete
//! round. This adapter lets the app show confirmed results while later queries
//! are still waiting for their request allowance.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};
use std::time::Duration;

use apw_core::{
    apple::{ApiError, Fetcher, StoreAvailability},
    model::{Region, UnknownReason},
};
use tokio::sync::mpsc;

use crate::store::now_ms;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Scope {
    Store(String),
    Nearby(String),
}

#[derive(Debug)]
pub enum FetchEvent {
    Started {
        generation: u64,
        scope: Scope,
        locale: String,
        parts: Vec<String>,
        at: u64,
        wait_until: Option<u64>,
    },
    Finished {
        generation: u64,
        scope: Scope,
        locale: String,
        parts: Vec<String>,
        at: u64,
        result: Result<Vec<StoreAvailability>, UnknownReason>,
    },
}

#[derive(Clone)]
pub struct ProgressFetcher<F> {
    inner: F,
    events: mpsc::UnboundedSender<FetchEvent>,
    pub generation: Arc<AtomicU64>,
}

impl<F> ProgressFetcher<F> {
    pub fn new(
        inner: F,
        events: mpsc::UnboundedSender<FetchEvent>,
        generation: Arc<AtomicU64>,
    ) -> Self {
        Self {
            inner,
            events,
            generation,
        }
    }

    fn started(&self, region: &Region, scope: &Scope, parts: &[String], delay: Duration) -> u64 {
        // A completion keeps the generation it began in. The consumer can then
        // discard it after pause, removal, or a new monitoring run.
        let generation = self.generation.load(Ordering::Acquire);
        let at = now_ms();
        let wait_until = (!delay.is_zero())
            .then(|| at.saturating_add(u64::try_from(delay.as_millis()).unwrap_or(u64::MAX)));
        let _ = self.events.send(FetchEvent::Started {
            generation,
            scope: scope.clone(),
            locale: region.locale.to_owned(),
            parts: parts.to_vec(),
            at,
            wait_until,
        });
        generation
    }

    fn finished(
        &self,
        generation: u64,
        region: &Region,
        scope: Scope,
        parts: &[String],
        result: Result<Vec<StoreAvailability>, UnknownReason>,
    ) {
        let _ = self.events.send(FetchEvent::Finished {
            generation,
            scope,
            locale: region.locale.to_owned(),
            parts: parts.to_vec(),
            at: now_ms(),
            result,
        });
    }
}

impl<F: Fetcher> Fetcher for ProgressFetcher<F> {
    async fn pickup_message(
        &self,
        region: &'static Region,
        store_number: &str,
        parts: &[String],
    ) -> Result<StoreAvailability, ApiError> {
        let scope = Scope::Store(store_number.to_owned());
        let delay = self.inner.pacing_delay(1).await;
        let generation = self.started(region, &scope, parts, delay);
        let result = self.inner.pickup_message(region, store_number, parts).await;
        self.finished(
            generation,
            region,
            scope,
            parts,
            result
                .as_ref()
                .map(|store| vec![store.clone()])
                .map_err(unknown_reason),
        );
        result
    }

    async fn pickup_message_nearby(
        &self,
        region: &'static Region,
        location: &str,
        parts: &[String],
    ) -> Result<Vec<StoreAvailability>, ApiError> {
        let scope = Scope::Nearby(location.to_owned());
        let delay = self.inner.pacing_delay(1).await;
        let generation = self.started(region, &scope, parts, delay);
        let result = self
            .inner
            .pickup_message_nearby(region, location, parts)
            .await;
        self.finished(
            generation,
            region,
            scope,
            parts,
            result.as_ref().cloned().map_err(unknown_reason),
        );
        result
    }

    async fn pacing_delay(&self, _requests: usize) -> Duration {
        // AppleClient still charges every real request. Pre-filling a whole
        // round is impossible when it is larger than the bucket capacity and
        // unnecessarily delays the next round before its per-request waits.
        self.inner.pacing_delay(1).await
    }
}

fn unknown_reason(error: &ApiError) -> UnknownReason {
    match error {
        ApiError::Blocked(detail) => UnknownReason::Blocked {
            detail: detail.clone(),
        },
        ApiError::RateLimited(_) => UnknownReason::RateLimited,
        ApiError::SchemaDrift { field, raw } => UnknownReason::SchemaDrift {
            field: field.clone(),
            raw: raw.clone(),
        },
        ApiError::Apple(message) => UnknownReason::AppleError {
            message: message.clone(),
        },
        ApiError::Transport(detail) => UnknownReason::Transport {
            detail: detail.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apw_core::{
        apple::PartStatus,
        model::{Availability, region_by_locale},
    };
    use std::{collections::BTreeMap, sync::atomic::AtomicUsize};
    use tokio::sync::Notify;

    #[derive(Clone)]
    struct FakeFetcher {
        release: Arc<Notify>,
        paced_requests: Arc<AtomicUsize>,
    }

    impl FakeFetcher {
        fn new() -> Self {
            Self {
                release: Arc::new(Notify::new()),
                paced_requests: Arc::new(AtomicUsize::new(0)),
            }
        }
    }

    impl Fetcher for FakeFetcher {
        async fn pickup_message(
            &self,
            _region: &'static Region,
            store_number: &str,
            parts: &[String],
        ) -> Result<StoreAvailability, ApiError> {
            if store_number == "slow" {
                self.release.notified().await;
            }
            if store_number == "blocked" {
                return Err(ApiError::Blocked("HTTP 541".into()));
            }
            Ok(StoreAvailability {
                store_number: store_number.to_owned(),
                store_name: store_number.to_owned(),
                parts: parts
                    .iter()
                    .map(|part| {
                        (
                            part.clone(),
                            PartStatus {
                                part_number: part.clone(),
                                availability: Availability::InStock,
                                product_title: None,
                                pickup_display: "available".into(),
                            },
                        )
                    })
                    .collect::<BTreeMap<_, _>>(),
            })
        }

        async fn pickup_message_nearby(
            &self,
            region: &'static Region,
            location: &str,
            parts: &[String],
        ) -> Result<Vec<StoreAvailability>, ApiError> {
            self.pickup_message(region, location, parts)
                .await
                .map(|s| vec![s])
        }

        async fn pacing_delay(&self, requests: usize) -> Duration {
            self.paced_requests.store(requests, Ordering::Release);
            Duration::from_secs(42)
        }
    }

    #[tokio::test]
    async fn first_result_is_published_while_later_query_is_still_waiting() {
        let inner = FakeFetcher::new();
        let (events, mut receiver) = mpsc::unbounded_channel();
        let fetcher = ProgressFetcher::new(inner.clone(), events, Arc::new(AtomicU64::new(7)));
        let region = region_by_locale("en_AU").unwrap();
        let task = tokio::spawn(async move {
            let parts = vec!["PRODUCT".to_owned()];
            fetcher
                .pickup_message(region, "first", &parts)
                .await
                .unwrap();
            fetcher
                .pickup_message(region, "slow", &parts)
                .await
                .unwrap();
        });
        assert!(
            matches!(receiver.recv().await.unwrap(), FetchEvent::Started {generation:7,scope:Scope::Store(s),..} if s == "first")
        );
        match receiver.recv().await.unwrap() {
            FetchEvent::Finished {
                generation: 7,
                result: Ok(stores),
                at,
                ..
            } => {
                assert_eq!(stores[0].store_number, "first");
                assert!(stores[0].parts["PRODUCT"].availability.is_in_stock());
                assert!(at > 0);
            }
            other => panic!("Expected the first result immediately, got {other:?}"),
        }
        assert!(
            matches!(receiver.recv().await.unwrap(), FetchEvent::Started {scope:Scope::Store(s),..} if s == "slow")
        );
        assert!(!task.is_finished());
        assert!(receiver.try_recv().is_err());
        inner.release.notify_one();
        task.await.unwrap();
        assert!(matches!(
            receiver.recv().await.unwrap(),
            FetchEvent::Finished { result: Ok(_), .. }
        ));
    }

    #[tokio::test]
    async fn completion_keeps_original_generation_and_nearby_metadata() {
        let inner = FakeFetcher::new();
        let generation = Arc::new(AtomicU64::new(3));
        let (events, mut receiver) = mpsc::unbounded_channel();
        let fetcher = ProgressFetcher::new(inner.clone(), events, generation.clone());
        let task = tokio::spawn(async move {
            fetcher
                .pickup_message_nearby(region_by_locale("en_AU").unwrap(), "slow", &["SKU".into()])
                .await
                .unwrap()
        });
        assert!(
            matches!(receiver.recv().await.unwrap(), FetchEvent::Started {generation:3,locale,parts,scope:Scope::Nearby(location),..} if locale == "en_AU" && parts == ["SKU"] && location == "slow")
        );
        generation.store(4, Ordering::Release);
        inner.release.notify_one();
        task.await.unwrap();
        assert!(
            matches!(receiver.recv().await.unwrap(), FetchEvent::Finished {generation:3,scope:Scope::Nearby(location),..} if location == "slow")
        );
    }

    #[tokio::test]
    async fn pacing_only_reserves_for_the_next_request() {
        let inner = FakeFetcher::new();
        let (events, _) = mpsc::unbounded_channel();
        let fetcher = ProgressFetcher::new(inner.clone(), events, Arc::new(AtomicU64::new(0)));
        assert_eq!(fetcher.pacing_delay(9).await, Duration::from_secs(42));
        assert_eq!(inner.paced_requests.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn blocked_result_remains_an_error_and_is_reported_as_unknown() {
        let (events, mut receiver) = mpsc::unbounded_channel();
        let fetcher = ProgressFetcher::new(FakeFetcher::new(), events, Arc::new(AtomicU64::new(0)));
        let result = fetcher
            .pickup_message(
                region_by_locale("en_AU").unwrap(),
                "blocked",
                &["SKU".into()],
            )
            .await;
        assert!(matches!(result,Err(ApiError::Blocked(detail)) if detail == "HTTP 541"));
        let _ = receiver.recv().await;
        assert!(
            matches!(receiver.recv().await.unwrap(), FetchEvent::Finished {result:Err(UnknownReason::Blocked{detail}),..} if detail == "HTTP 541")
        );
    }
}
