// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::sync::RwLock;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::config::Config;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct ActivityPolicy {
    pub activity_source: bool,
    pub activity_titles: bool,
    pub activity_urls: bool,
    pub activity_exclude_apps: Vec<String>,
    pub activity_exclude_titles: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityPrefsSnapshot {
    pub source: bool,
    pub titles: bool,
    pub urls: bool,
    pub exclude_apps: Vec<String>,
    pub exclude_titles: Vec<String>,
    pub generation: u64,
}

pub struct ActivityPrefs {
    state: RwLock<ActivityPrefsSnapshot>,
    generation: AtomicU64,
}

impl ActivityPrefs {
    pub fn new(config: &Config) -> Self {
        let initial_gen = 1;
        let snapshot = ActivityPrefsSnapshot {
            source: config.activity_source,
            titles: config.activity_titles,
            urls: config.activity_urls,
            exclude_apps: config.activity_exclude_apps.clone(),
            exclude_titles: config.activity_exclude_titles.clone(),
            generation: initial_gen,
        };
        Self {
            state: RwLock::new(snapshot),
            generation: AtomicU64::new(initial_gen),
        }
    }

    pub fn snapshot(&self) -> ActivityPrefsSnapshot {
        let lock = self.state.read().unwrap_or_else(|p| p.into_inner());
        lock.clone()
    }

    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    pub fn apply_policy(&self, policy: ActivityPolicy) -> u64 {
        let mut lock = self.state.write().unwrap_or_else(|p| p.into_inner());
        let next = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        lock.source = policy.activity_source;
        lock.titles = policy.activity_titles;
        lock.urls = policy.activity_urls;
        lock.exclude_apps = policy.activity_exclude_apps;
        lock.exclude_titles = policy.activity_exclude_titles;
        lock.generation = next;
        next
    }

    pub fn is_app_excluded(&self, app: &str, app_id: Option<&str>) -> bool {
        let lock = self.state.read().unwrap_or_else(|p| p.into_inner());
        for excluded in &lock.exclude_apps {
            if excluded == app || app_id.is_some_and(|id| excluded == id) {
                return true;
            }
        }
        false
    }

    pub fn is_title_excluded(&self, title: &str) -> bool {
        let lock = self.state.read().unwrap_or_else(|p| p.into_inner());
        for excluded in &lock.exclude_titles {
            if excluded == title {
                return true;
            }
        }
        false
    }

    pub fn has_title_exclusions(&self) -> bool {
        let lock = self.state.read().unwrap_or_else(|p| p.into_inner());
        !lock.exclude_titles.is_empty()
    }

    /// Drops outstanding reads. The next sample starts a new interval.
    pub fn bump_generation(&self) -> u64 {
        let mut lock = self.state.write().unwrap_or_else(|p| p.into_inner());
        let next = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        lock.generation = next;
        next
    }
}
