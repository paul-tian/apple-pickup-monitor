//! Per-response observations. The upstream watcher only publishes whole rounds;
//! this projection preserves the time each individual Apple response arrived.
use crate::{
    progress::{FetchEvent, Scope},
    store::now_ms,
};
use apw_core::{
    catalog::Catalog,
    model::{Availability, Target, UnknownReason},
    watcher::TargetState,
};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub completed_stores: usize,
    pub total_stores: usize,
    pub active_location: Option<String>,
    pub started_at: u64,
    pub phase: &'static str,
    pub wait_until: Option<u64>,
    #[serde(skip)]
    scope: Option<Scope>,
    #[serde(skip)]
    scope_locale: Option<String>,
}
#[derive(Default)]
pub struct Observations {
    generation: u64,
    reset_at: u64,
    rows: HashMap<String, TargetState>,
    seen: HashSet<String>,
    pub progress: Option<Progress>,
}
impl Observations {
    pub fn reset(&mut self, targets: &[Target], generation: u64) {
        self.generation = generation;
        self.reset_at = now_ms();
        self.progress = None;
        self.seen.clear();
        self.rows
            .retain(|key, _| targets.iter().any(|t| t.key().to_string() == *key));
    }
    pub fn row(&self, target: &Target) -> TargetState {
        let mut row = self
            .rows
            .get(&target.key().to_string())
            .cloned()
            .unwrap_or_else(|| TargetState {
                target: target.clone(),
                availability: Availability::Unknown(UnknownReason::NotYetChecked),
                last_checked_ms: None,
                consecutive_failures: 0,
            });
        row.target = target.clone();
        row
    }
    pub fn check_phase(&self, target: &Target) -> Option<&'static str> {
        self.progress.as_ref().map(|p| {
            if p.scope_locale.as_deref() == Some(target.locale.as_str())
                && p.scope.as_ref().is_some_and(|s| matches_scope(s, target))
            {
                "checking"
            } else {
                "queued"
            }
        })
    }
    pub fn apply(&mut self, event: FetchEvent, targets: &[Target]) -> Vec<TargetState> {
        match event {
            FetchEvent::Started {
                generation,
                scope,
                locale,
                parts,
                at,
                wait_until,
            } => {
                if generation != self.generation
                    || !targets
                        .iter()
                        .any(|t| t.locale == locale && parts.contains(&t.part_number))
                {
                    return vec![];
                }
                if self.progress.is_none() {
                    self.seen.clear();
                    self.progress = Some(Progress {
                        completed_stores: 0,
                        total_stores: targets
                            .iter()
                            .map(|t| (&t.locale, &t.store_number))
                            .collect::<HashSet<_>>()
                            .len(),
                        active_location: None,
                        started_at: at,
                        phase: "checking",
                        wait_until: None,
                        scope: None,
                        scope_locale: None,
                    });
                }
                let p = self.progress.as_mut().unwrap();
                p.active_location = Some(match &scope {
                    Scope::Nearby(l) | Scope::Store(l) => l.clone(),
                });
                p.scope = Some(scope);
                p.scope_locale = Some(locale);
                p.wait_until = wait_until;
                p.phase = if wait_until.is_some_and(|t| t > now_ms()) {
                    "waiting"
                } else {
                    "checking"
                };
                vec![]
            }
            FetchEvent::Finished {
                generation,
                scope,
                locale,
                parts,
                at,
                result,
            } => {
                if generation != self.generation {
                    return vec![];
                }
                let mut rows = vec![];
                for target in targets
                    .iter()
                    .filter(|t| t.locale == locale && parts.contains(&t.part_number))
                {
                    let availability = match &result {
                        Ok(stores) => {
                            // Nearby responses can also cover a later query's store.
                            let Some(store) = stores
                                .iter()
                                .find(|s| s.store_number == target.store_number)
                            else {
                                continue;
                            };
                            store
                                .parts
                                .get(&target.part_number)
                                .map(|p| p.availability.clone())
                                .unwrap_or_else(|| {
                                    Availability::Unknown(UnknownReason::SchemaDrift {
                                        field: "partsAvailability".into(),
                                        raw: "Requested part is missing".into(),
                                    })
                                })
                        }
                        Err(reason) if matches_scope(&scope, target) => {
                            Availability::Unknown(reason.clone())
                        }
                        Err(_) => continue,
                    };
                    let old = self.row(target);
                    let row = TargetState {
                        target: target.clone(),
                        consecutive_failures: if availability.is_failure() {
                            old.consecutive_failures.saturating_add(1)
                        } else {
                            0
                        },
                        availability,
                        last_checked_ms: Some(at),
                    };
                    self.seen.insert(target.key().to_string());
                    self.rows.insert(target.key().to_string(), row.clone());
                    rows.push(row);
                }
                if let Some(p) = &mut self.progress {
                    let store_ids: HashSet<_> = targets
                        .iter()
                        .map(|t| (&t.locale, &t.store_number))
                        .collect();
                    p.completed_stores = store_ids
                        .iter()
                        .filter(|id| {
                            targets
                                .iter()
                                .filter(|t| (&t.locale, &t.store_number) == **id)
                                .all(|t| self.seen.contains(&t.key().to_string()))
                        })
                        .count();
                    p.phase = "waiting";
                    p.wait_until = None;
                    p.active_location = None;
                    p.scope = None;
                    p.scope_locale = None;
                }
                rows
            }
        }
    }
    pub fn finish_cycle(&mut self, snapshot: &[TargetState]) -> Vec<TargetState> {
        // Never replace response timestamps with the upstream end-of-round time.
        if self.progress.is_none() {
            return vec![];
        }
        let mut missing = vec![];
        for state in snapshot {
            if self.seen.contains(&state.target.key().to_string()) {
                continue;
            }
            let availability = if state.availability.is_unknown() {
                state.availability.clone()
            } else {
                Availability::Unknown(UnknownReason::Transport {
                    detail: "No response was observed for this check".into(),
                })
            };
            let row = TargetState {
                target: state.target.clone(),
                availability,
                last_checked_ms: Some(now_ms()),
                consecutive_failures: state.consecutive_failures,
            };
            self.rows.insert(row.target.key().to_string(), row.clone());
            missing.push(row);
        }
        self.progress = None;
        missing
    }
    pub fn accepts_cycle(&self, snapshot: &[TargetState]) -> bool {
        self.progress.is_some()
            && snapshot
                .iter()
                .filter_map(|s| s.last_checked_ms)
                .max()
                .is_some_and(|at| at > self.reset_at)
    }
}
fn matches_scope(scope: &Scope, target: &Target) -> bool {
    match scope {
        Scope::Store(number) => target.store_number == *number,
        Scope::Nearby(location) => target.pickup_location.as_ref() == Some(location),
    }
}

/// Use the core's locale-specific city/postcode rules globally. Australia also
/// shares a verified state-level anchor, with every returned store checked by ID.
/// The upstream watcher individually queries any store missing from that response.
pub fn group_nearby_targets(targets: &mut [Target], catalog: &Catalog) {
    let mut anchors = HashMap::new();
    for target in targets {
        let Some(store) = catalog.store_by_number(&target.locale, &target.store_number) else {
            continue;
        };
        let Some(location) = store.pickup_location(&target.locale) else {
            continue;
        };
        let state = store.state.trim();
        if target.locale != "en_AU" || state.is_empty() {
            target.pickup_location = Some(location);
            continue;
        }
        let anchor = anchors
            .entry((target.locale.clone(), state.to_string()))
            .or_insert(location);
        target.pickup_location = Some(anchor.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use apw_core::apple::{PartStatus, StoreAvailability};
    #[test]
    fn identical_store_ids_in_different_regions_do_not_share_progress_or_results() {
        let first = target("A", "Same city");
        let mut second = first.clone();
        second.locale = "en_US".into();
        let targets = [first, second];
        let mut state = Observations::default();
        state.reset(&targets, 1);
        state.apply(
            FetchEvent::Started {
                generation: 1,
                scope: Scope::Nearby("Same city".into()),
                locale: "en_AU".into(),
                parts: vec!["SKU".into()],
                at: 100,
                wait_until: None,
            },
            &targets,
        );
        assert_eq!(state.progress.as_ref().unwrap().total_stores, 2);
        assert_eq!(state.check_phase(&targets[0]), Some("checking"));
        assert_eq!(state.check_phase(&targets[1]), Some("queued"));
        state.apply(
            FetchEvent::Finished {
                generation: 1,
                scope: Scope::Nearby("Same city".into()),
                locale: "en_AU".into(),
                parts: vec!["SKU".into()],
                at: 200,
                result: Ok(vec![result("A")]),
            },
            &targets,
        );
        assert!(state.row(&targets[0]).availability.is_in_stock());
        assert!(state.row(&targets[1]).availability.is_unknown());
        assert_eq!(state.progress.as_ref().unwrap().completed_stores, 1);
    }
    #[test]
    fn non_australian_cities_keep_upstream_query_locations() {
        let catalog = Catalog::new();
        let stores = catalog.stores("en_US").unwrap();
        let (first, second) = stores
            .iter()
            .find_map(|a| {
                stores
                    .iter()
                    .find(|b| {
                        a.state == b.state
                            && a.number != b.number
                            && a.pickup_location("en_US") != b.pickup_location("en_US")
                    })
                    .map(|b| (a, b))
            })
            .unwrap();
        let mut targets = [target(&first.number, "old"), target(&second.number, "old")];
        for t in &mut targets {
            t.locale = "en_US".into();
        }
        group_nearby_targets(&mut targets, &catalog);
        assert_eq!(targets[0].pickup_location, first.pickup_location("en_US"));
        assert_eq!(targets[1].pickup_location, second.pickup_location("en_US"));
        assert_ne!(targets[0].pickup_location, targets[1].pickup_location);
    }
    fn target(number: &str, location: &str) -> Target {
        Target {
            locale: "en_AU".into(),
            store_number: number.into(),
            store_title: number.into(),
            part_number: "SKU".into(),
            product_name: "Phone".into(),
            companion_part: None,
            pickup_location: Some(location.into()),
        }
    }
    fn result(number: &str) -> StoreAvailability {
        StoreAvailability {
            store_number: number.into(),
            store_name: number.into(),
            parts: [(
                "SKU".into(),
                PartStatus {
                    part_number: "SKU".into(),
                    availability: Availability::InStock,
                    product_title: None,
                    pickup_display: "available".into(),
                },
            )]
            .into(),
        }
    }
    #[test]
    fn partial_results_preserve_times_and_never_guess_missing_store() {
        let targets = [target("A", "Sydney"), target("B", "Sydney")];
        let mut state = Observations::default();
        state.reset(&targets, 1);
        state.apply(
            FetchEvent::Started {
                generation: 1,
                scope: Scope::Nearby("Sydney".into()),
                locale: "en_AU".into(),
                parts: vec!["SKU".into()],
                at: 100,
                wait_until: None,
            },
            &targets,
        );
        let rows = state.apply(
            FetchEvent::Finished {
                generation: 1,
                scope: Scope::Nearby("Sydney".into()),
                locale: "en_AU".into(),
                parts: vec!["SKU".into()],
                at: 200,
                result: Ok(vec![result("A")]),
            },
            &targets,
        );
        assert_eq!(rows.len(), 1);
        assert!(state.row(&targets[0]).availability.is_in_stock());
        assert_eq!(state.row(&targets[0]).last_checked_ms, Some(200));
        assert!(state.row(&targets[1]).availability.is_unknown());
        state.finish_cycle(&[TargetState {
            target: targets[0].clone(),
            availability: Availability::InStock,
            last_checked_ms: Some(900),
            consecutive_failures: 0,
        }]);
        assert_eq!(state.row(&targets[0]).last_checked_ms, Some(200));
    }
    #[test]
    fn stopped_generation_cannot_publish_late_results() {
        let targets = [target("A", "Sydney")];
        let mut state = Observations::default();
        state.reset(&targets, 2);
        assert!(
            state
                .apply(
                    FetchEvent::Finished {
                        generation: 1,
                        scope: Scope::Store("A".into()),
                        locale: "en_AU".into(),
                        parts: vec!["SKU".into()],
                        at: 200,
                        result: Ok(vec![result("A")])
                    },
                    &targets
                )
                .is_empty()
        );
        assert!(state.row(&targets[0]).availability.is_unknown());
    }
    #[test]
    fn nsw_stores_share_an_anchor_but_different_states_do_not() {
        let mut targets = vec![
            target("R238", "Sydney"),
            target("R253", "Chatswood"),
            target("R385", "Charlestown"),
            target("R180", "Chadstone"),
        ];
        group_nearby_targets(&mut targets, &Catalog::new());
        assert_eq!(targets[0].pickup_location, targets[1].pickup_location);
        assert_eq!(targets[0].pickup_location, targets[2].pickup_location);
        assert_ne!(targets[0].pickup_location, targets[3].pickup_location);
    }
}
