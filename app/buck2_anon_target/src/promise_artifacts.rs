/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under both the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree and the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree.
 */

use std::collections::HashMap;
use std::fmt::Debug;
use std::sync::Arc;
use std::sync::OnceLock;

use allocative::Allocative;
use buck2_build_api::artifact_groups::promise::PromiseArtifact;
use buck2_build_api::artifact_groups::promise::PromiseArtifactId;
use buck2_build_api::artifact_groups::promise::PromiseArtifactResolveError;
use buck2_build_api::interpreter::rule_defs::artifact::ValueAsArtifactLike;
use buck2_core::fs::paths::forward_rel_path::ForwardRelativePathBuf;
use buck2_error::Context;
use buck2_interpreter::starlark_promise::StarlarkPromise;
use dupe::Dupe;
use starlark::codemap::FileSpan;
use starlark::values::Trace;
use starlark::values::UnpackValue;
use starlark::values::ValueTyped;

#[derive(Debug, Trace, Allocative)]
struct PromiseArtifactEntry {
    location: Option<FileSpan>,
    artifact: PromiseArtifact,
}

/// The PromiseArtifactRegistry stores promises registered with `artifact_promise_mappings` in `anon_rule()`, and their
/// corresponding internal PromiseArtifact. At the end of analysis (after promises have been resolved),
/// all PromiseArtifact will be updated to have the resolved artifact from the corresponding starlark promise.
#[derive(Debug, Trace, Allocative)]
pub(crate) struct PromiseArtifactRegistry<'v> {
    promises: Vec<ValueTyped<'v, StarlarkPromise<'v>>>,
    artifacts: Vec<PromiseArtifactEntry>,
}

impl<'v> PromiseArtifactRegistry<'v> {
    pub(crate) fn new() -> Self {
        Self {
            promises: Vec::new(),
            artifacts: Vec::new(),
        }
    }

    pub(crate) fn resolve_all(
        &self,
        short_paths: &HashMap<PromiseArtifactId, ForwardRelativePathBuf>,
    ) -> anyhow::Result<()> {
        for (promise, artifact_entry) in std::iter::zip(&self.promises, &self.artifacts) {
            match promise.get() {
                Some(v) => match ValueAsArtifactLike::unpack_value(v) {
                    Some(v) => {
                        let short_path = short_paths.get(artifact_entry.artifact.id()).cloned();

                        if let Some(artifact) = v.0.get_associated_artifacts() {
                            if !artifact.is_empty() {
                                return Err(
                                    PromiseArtifactResolveError::HasAssociatedArtifacts.into()
                                );
                            }
                        }
                        let artifact =
                            v.0.get_bound_artifact()
                                .context("expected bound artifact for promise_artifact resolve")?;
                        artifact_entry.artifact.resolve(artifact, &short_path)?;
                    }
                    None => {
                        return Err(PromiseArtifactResolveError::NotAnArtifact(
                            artifact_entry.location.clone(),
                            v.to_repr(),
                        )
                        .into());
                    }
                },
                None => {
                    return Err(PromiseArtifactResolveError::PromiseNotResolved(
                        artifact_entry.location.clone(),
                        promise.to_string(),
                    )
                    .into());
                }
            }
        }
        Ok(())
    }

    pub(crate) fn register(
        &mut self,
        promise: ValueTyped<'v, StarlarkPromise<'v>>,
        location: Option<FileSpan>,
        id: PromiseArtifactId,
    ) -> anyhow::Result<PromiseArtifact> {
        let artifact = PromiseArtifact::new(Arc::new(OnceLock::new()), Arc::new(id));

        self.promises.push(promise);
        self.artifacts.push(PromiseArtifactEntry {
            location,
            artifact: artifact.dupe(),
        });
        Ok(artifact)
    }
}
