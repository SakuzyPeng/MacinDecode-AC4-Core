//! 可复制控制候选；独立 Session 与 Scene 共用相同的 prepare/commit 门禁。
use crate::{AccessUnitContext, MetadataError, MetadataErrorContext, MetadataErrorKind};
use macindecode_ac4_bitstream::topology::{
    Ac4Topology, ResetReason, TopologyStateMachine, TopologyTransition,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MetadataStateMachine {
    topology: TopologyStateMachine,
}
impl Default for MetadataStateMachine {
    fn default() -> Self {
        Self::new()
    }
}
impl MetadataStateMachine {
    pub const fn new() -> Self {
        Self {
            topology: TopologyStateMachine::new(),
        }
    }
    pub fn prepare(
        &self,
        topology: &Ac4Topology,
        context: AccessUnitContext,
    ) -> Result<PreparedMetadataState, MetadataError> {
        let mut next = *self;
        if context.discontinuity() {
            next.mark_discontinuity(ResetReason::ExternalDiscontinuity);
        }
        let transition = next.observe(topology);
        if transition.config_changed && self.generation() == u32::MAX {
            return Err(MetadataError::new(
                MetadataErrorKind::TimelineOverflow,
                MetadataErrorContext::for_access_unit(context.index()),
            ));
        }
        Ok(PreparedMetadataState { next, transition })
    }
    pub fn commit(&mut self, prepared: PreparedMetadataState) {
        *self = prepared.next;
    }
    pub fn observe(&mut self, topology: &Ac4Topology) -> TopologyTransition {
        self.topology.observe(topology)
    }
    pub fn mark_discontinuity(&mut self, reason: ResetReason) {
        self.topology.mark_discontinuity(reason);
    }
    pub const fn generation(&self) -> u32 {
        self.topology.generation()
    }
    pub const fn is_waiting_for_random_access(&self) -> bool {
        self.topology.is_waiting_for_random_access()
    }
}
#[derive(Debug, Clone, Copy)]
pub struct PreparedMetadataState {
    next: MetadataStateMachine,
    transition: TopologyTransition,
}
impl PreparedMetadataState {
    pub const fn transition(&self) -> TopologyTransition {
        self.transition
    }
    pub const fn next(&self) -> MetadataStateMachine {
        self.next
    }
}
