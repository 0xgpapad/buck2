/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use std::sync::Arc;

use allocative::Allocative;
use buck2_core::base_deferred_key_dyn::BaseDeferredKeyDyn;
use buck2_core::base_deferred_key_dyn::BaseDeferredKeyDynImpl;
use buck2_core::target::label::ConfiguredTargetLabel;
use buck2_data::action_key_owner::BaseDeferredKeyProto;
use buck2_data::ToProtoMessage;
use derive_more::Display;
use dupe::Dupe;
use gazebo::variants::UnpackVariants;

use crate::analysis::anon_target_node::AnonTarget;
use crate::bxl::types::BxlKey;

/// Key types for the base 'DeferredKey'
#[derive(
    Clone,
    Dupe,
    Display,
    Debug,
    Eq,
    Hash,
    PartialEq,
    UnpackVariants,
    Allocative
)]
pub enum BaseDeferredKey {
    TargetLabel(ConfiguredTargetLabel),
    AnonTarget(Arc<AnonTarget>),
    BxlLabel(BxlKey),
}

impl BaseDeferredKey {
    pub fn into_dyn(self) -> BaseDeferredKeyDyn {
        match self {
            BaseDeferredKey::TargetLabel(label) => BaseDeferredKeyDyn::TargetLabel(label),
            BaseDeferredKey::AnonTarget(target) => BaseDeferredKeyDyn::AnonTarget(target),
            BaseDeferredKey::BxlLabel(label) => {
                BaseDeferredKeyDyn::BxlLabel(label.into_base_deferred_key_dyn_impl())
            }
        }
    }

    #[allow(dead_code)] // TODO(nga): used in the following diff D45926684.
    pub(crate) fn from_dyn(key_dyn: BaseDeferredKeyDyn) -> BaseDeferredKey {
        match key_dyn {
            BaseDeferredKeyDyn::TargetLabel(label) => BaseDeferredKey::TargetLabel(label),
            BaseDeferredKeyDyn::AnonTarget(target) => {
                BaseDeferredKey::AnonTarget(target.into_any().downcast().unwrap())
            }
            BaseDeferredKeyDyn::BxlLabel(label) => {
                BaseDeferredKey::BxlLabel(BxlKey::from_base_deferred_key_dyn_impl(label).unwrap())
            }
        }
    }

    pub fn to_proto(&self) -> BaseDeferredKeyProto {
        match self {
            BaseDeferredKey::TargetLabel(t) => BaseDeferredKeyProto::TargetLabel(t.as_proto()),
            BaseDeferredKey::AnonTarget(a) => a.to_proto(),
            BaseDeferredKey::BxlLabel(b) => BaseDeferredKeyProto::BxlKey(b.as_proto()),
        }
    }
}
