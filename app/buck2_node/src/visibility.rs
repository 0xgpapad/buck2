/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use std::fmt;
use std::fmt::Display;

use allocative::Allocative;
use buck2_core::pattern::pattern_type::TargetPatternExtra;
use buck2_core::pattern::ParsedPattern;
use buck2_core::target::label::TargetLabel;
use buck2_util::arc_str::ThinArcSlice;
use dupe::Dupe;
use gazebo::prelude::SliceExt;
use thiserror::Error;

use crate::attrs::attr_type::any_matches::AnyMatches;

#[derive(Debug, Error)]
pub enum VisibilityError {
    #[error(
        "`{0}` is not visible to `{1}` (run `buck2 uquery --output-attribute visibility {0}` to check the visibility)"
    )]
    NotVisibleTo(TargetLabel, TargetLabel),
}

#[derive(Debug, Eq, PartialEq, Hash, Clone, Allocative, derive_more::Display)]
pub struct VisibilityPattern(pub ParsedPattern<TargetPatternExtra>);

impl VisibilityPattern {
    pub const PUBLIC: &'static str = "PUBLIC";

    pub fn testing_new(pattern: &str) -> VisibilityPattern {
        VisibilityPattern(ParsedPattern::testing_parse(pattern))
    }
}

/// Represents the visibility spec of a target. Note that targets in the same package will ignore the
/// visibility spec of each other.
#[derive(Debug, Eq, PartialEq, Hash, Clone, Dupe, Allocative)]
pub enum VisibilitySpecification {
    Public,
    // Default is used when a target doesn't specify any visibility.
    VisibleTo(ThinArcSlice<VisibilityPattern>),
}

impl Default for VisibilitySpecification {
    fn default() -> Self {
        Self::DEFAULT
    }
}

#[derive(Default, Debug, Eq, PartialEq, Hash, Clone, Dupe, Allocative)]
pub enum WithinViewSpecification {
    // Default is used when a target doesn't specify any visibility.
    #[default]
    Public,
    VisibleTo(ThinArcSlice<VisibilityPattern>),
}

impl VisibilitySpecification {
    pub const DEFAULT: VisibilitySpecification =
        VisibilitySpecification::VisibleTo(ThinArcSlice::empty());

    pub(crate) fn is_default(&self) -> bool {
        match self {
            VisibilitySpecification::Public => false,
            VisibilitySpecification::VisibleTo(patterns) => patterns.is_empty(),
        }
    }

    pub fn is_visible_to(&self, target: &TargetLabel) -> bool {
        match self {
            VisibilitySpecification::Public => true,
            VisibilitySpecification::VisibleTo(patterns) => {
                for pattern in patterns {
                    if pattern.0.matches(target) {
                        return true;
                    }
                }
                false
            }
        }
    }

    pub(crate) fn to_json(&self) -> serde_json::Value {
        let list = match self {
            VisibilitySpecification::Public => vec![serde_json::Value::String(
                VisibilityPattern::PUBLIC.to_owned(),
            )],
            VisibilitySpecification::VisibleTo(patterns) => {
                patterns.map(|p| serde_json::Value::String(p.to_string()))
            }
        };
        serde_json::Value::Array(list)
    }

    pub fn extend_with(&self, other: &VisibilitySpecification) -> VisibilitySpecification {
        match (self, other) {
            (VisibilitySpecification::Public, _) | (_, VisibilitySpecification::Public) => {
                VisibilitySpecification::Public
            }
            (
                VisibilitySpecification::VisibleTo(this),
                VisibilitySpecification::VisibleTo(other),
            ) => VisibilitySpecification::VisibleTo(this.iter().chain(other).cloned().collect()),
        }
    }

    pub fn testing_parse(patterns: &[&str]) -> VisibilitySpecification {
        if patterns.contains(&VisibilityPattern::PUBLIC) {
            VisibilitySpecification::Public
        } else {
            VisibilitySpecification::VisibleTo(
                patterns
                    .iter()
                    .map(|p| VisibilityPattern::testing_new(p))
                    .collect(),
            )
        }
    }
}

impl Display for VisibilitySpecification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VisibilitySpecification::Public => write!(f, "[\"{}\"]", VisibilityPattern::PUBLIC),
            VisibilitySpecification::VisibleTo(patterns) => {
                write!(f, "[")?;
                for (i, pattern) in patterns.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "\"{}\"", pattern)?;
                }
                write!(f, "]")
            }
        }
    }
}

impl WithinViewSpecification {
    pub fn extend_with(&self, other: &WithinViewSpecification) -> WithinViewSpecification {
        match (self, other) {
            (WithinViewSpecification::Public, _) | (_, WithinViewSpecification::Public) => {
                WithinViewSpecification::Public
            }
            (
                WithinViewSpecification::VisibleTo(this),
                WithinViewSpecification::VisibleTo(other),
            ) => WithinViewSpecification::VisibleTo(this.iter().chain(other).cloned().collect()),
        }
    }
}

impl AnyMatches for VisibilitySpecification {
    fn any_matches(&self, filter: &dyn Fn(&str) -> anyhow::Result<bool>) -> anyhow::Result<bool> {
        match self {
            VisibilitySpecification::Public => filter(VisibilityPattern::PUBLIC),
            visibility if visibility.is_default() => filter(":"),
            VisibilitySpecification::VisibleTo(patterns) => {
                for p in patterns {
                    if filter(&p.to_string())? {
                        return Ok(true);
                    }
                }
                Ok(false)
            }
        }
    }
}

pub struct VisibilityWithinViewBuilder {
    cap: usize,
    seen_public: bool,
    patterns: Option<Vec<VisibilityPattern>>,
}

impl VisibilityWithinViewBuilder {
    pub fn with_capacity(cap: usize) -> VisibilityWithinViewBuilder {
        VisibilityWithinViewBuilder {
            cap,
            seen_public: false,
            patterns: None,
        }
    }

    pub fn add_public(&mut self) {
        self.seen_public = true;
    }

    pub fn add(&mut self, pattern: VisibilityPattern) {
        if !self.seen_public {
            self.patterns
                .get_or_insert_with(|| Vec::with_capacity(self.cap))
                .push(pattern);
        }
    }

    pub fn build_visibility(self) -> VisibilitySpecification {
        if self.seen_public {
            VisibilitySpecification::Public
        } else {
            VisibilitySpecification::VisibleTo(ThinArcSlice::from_iter(
                self.patterns.unwrap_or_default(),
            ))
        }
    }

    pub fn build_within_view(self) -> WithinViewSpecification {
        if self.seen_public {
            WithinViewSpecification::Public
        } else {
            match self.patterns {
                None => WithinViewSpecification::Public,
                Some(patterns) => {
                    WithinViewSpecification::VisibleTo(ThinArcSlice::from_iter(patterns))
                }
            }
        }
    }
}
