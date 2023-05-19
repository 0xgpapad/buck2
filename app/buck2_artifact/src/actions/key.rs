/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use allocative::Allocative;
use buck2_core::base_deferred_key::BaseDeferredKey;
use buck2_data::ToProtoMessage;
use dupe::Dupe;

use crate::deferred::key::DeferredKey;

/// A key to look up an 'Action' from the 'ActionAnalysisResult'.
/// Since 'Action's are registered as 'Deferred's
#[derive(
    Debug,
    Eq,
    PartialEq,
    Hash,
    Clone,
    Dupe,
    derive_more::Display,
    Allocative
)]
pub struct ActionKey(
    /// `DeferredData<Arc<RegisteredAction>>`.
    DeferredKey,
);

impl ActionKey {
    pub fn unchecked_new(key: DeferredKey) -> ActionKey {
        ActionKey(key)
    }

    pub fn deferred_key(&self) -> &DeferredKey {
        &self.0
    }

    pub fn owner(&self) -> &BaseDeferredKey {
        self.deferred_key().owner()
    }
}

impl ToProtoMessage for ActionKey {
    type Message = buck2_data::ActionKey;

    fn as_proto(&self) -> Self::Message {
        buck2_data::ActionKey {
            id: self.deferred_key().id().as_usize().to_ne_bytes().to_vec(),
            owner: Some(self.deferred_key().owner().to_proto().into()),
            key: self.deferred_key().action_key(),
        }
    }
}
