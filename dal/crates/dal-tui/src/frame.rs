//! Live-block region budgets and bounded update/input coalescing.

use std::collections::VecDeque;

use dal_core::{Update, UpdateKind};

/// Priority of a narrow-terminal warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NarrowWarning {
    /// The terminal has fewer than eight rows.
    Rows,
    /// The terminal has fewer than twelve columns.
    Columns,
}

/// Requested content heights before the region cap is applied.
#[derive(Debug, Clone, Copy, Default)]
pub struct RegionRequest {
    /// Number of pending notices.
    pub notices: usize,
    /// Activity rows requested by tool and child-session cards.
    pub activity: usize,
    /// Composer rows requested by the current draft.
    pub composer: usize,
    /// Whether the ordinary hint row is visible.
    pub hint: bool,
    /// Natural overlay height, when one overlay owns composer and hint.
    pub overlay: Option<usize>,
}

/// Actual bottom-stack allocation for one frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegionBudget {
    /// Maximum inline live-block rows.
    pub cap: usize,
    /// Notice rows allocated at the top of the live block.
    pub notices: usize,
    /// Activity rows allocated below notices.
    pub activity: usize,
    /// Overlay rows, when an overlay is open.
    pub overlay: usize,
    /// Composer rows allocated above the hint.
    pub composer: usize,
    /// Hint rows.
    pub hint: usize,
    /// Always-painted status row count.
    pub status: usize,
    /// Narrow-size warning, when an interaction floor is active.
    pub warning: Option<NarrowWarning>,
}

impl RegionBudget {
    /// Allocates a bottom-anchored region stack for terminal dimensions and content.
    #[must_use]
    pub fn allocate(width: u16, height: u16, requested: RegionRequest) -> Self {
        let height = usize::from(height);
        let width = usize::from(width);
        if height < 8 {
            return Self::floor(NarrowWarning::Rows);
        }
        if width < 12 {
            return Self::floor(NarrowWarning::Columns);
        }

        let cap = (height - 4).min(16);
        let available = cap.saturating_sub(1);
        let (notice_cap, activity_cap, composer_cap, hint_cap) = match height {
            28.. => (3, 8, 8, 1),
            24..=27 => (2, 5, 6, 1),
            12..=23 => (1, 2, 3, 0),
            _ => (0, 0, 3, 0),
        };

        if let Some(natural_overlay) = requested.overlay {
            return overlay_budget(cap, available, requested.notices, natural_overlay);
        }

        let mut notices = requested.notices.min(notice_cap);
        let mut activity = requested.activity.min(activity_cap);
        let mut composer = requested.composer.max(1).min(composer_cap);
        let mut hint = usize::from(requested.hint).min(hint_cap);
        while notices + activity + composer + hint > available {
            if notices > 1 {
                notices = 1;
            } else if activity > 0 {
                activity -= 1;
            } else if composer > 1 {
                composer -= 1;
            } else if hint > 0 {
                hint = 0;
            } else {
                break;
            }
        }
        Self {
            cap,
            notices,
            activity,
            overlay: 0,
            composer,
            hint,
            status: 1,
            warning: None,
        }
    }

    fn floor(warning: NarrowWarning) -> Self {
        Self {
            cap: 2,
            notices: 1,
            activity: 0,
            overlay: 0,
            composer: 0,
            hint: 0,
            status: 1,
            warning: Some(warning),
        }
    }
}

fn overlay_budget(
    cap: usize,
    available: usize,
    notices_requested: usize,
    natural: usize,
) -> RegionBudget {
    let mut notices = notices_requested.min(1);
    let floor = 3;
    if available.saturating_sub(notices) < floor {
        notices = 0;
    }
    let overlay = natural.max(floor).min(available.saturating_sub(notices));
    let activity = available.saturating_sub(notices + overlay);
    RegionBudget {
        cap,
        notices,
        activity,
        overlay,
        composer: 0,
        hint: 0,
        status: 1,
        warning: None,
    }
}

/// Item classes that retain order or can be coalesced in the bounded queues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueueClass {
    /// A replaceable delta or progress observation.
    Replaceable,
    /// An event whose loss would leave clients out of sync.
    Lossless,
}

/// A lossless update the full update queue refused to drop an earlier one for.
#[derive(Debug, Clone, PartialEq)]
pub struct QueueFull(
    /// The update that was not enqueued.
    pub Box<Update>,
);

/// Bounded coalescer for updates and terminal input.
#[derive(Debug)]
pub struct Coalescer {
    updates: VecDeque<Update>,
    inputs: VecDeque<Vec<u8>>,
    shed: u64,
}

impl Default for Coalescer {
    fn default() -> Self {
        Self {
            updates: VecDeque::with_capacity(4096),
            inputs: VecDeque::with_capacity(256),
            shed: 0,
        }
    }
}

impl Coalescer {
    /// Enqueues an update, merging adjacent text deltas, replacing progress by call id, and shedding replaceable entries when the queue is full.
    ///
    /// # Errors
    /// Returns [`QueueFull`] with the update when the queue holds 4096 lossless
    /// events and `update` is lossless too; the queue is left unchanged so the
    /// caller drains it or resynchronises instead of losing a transition.
    pub fn push_update(&mut self, update: Update) -> Result<(), QueueFull> {
        if coalesce_update(&mut self.updates, &update) {
            return Ok(());
        }
        let incoming_class = classify(&update.kind);
        if self.updates.len() >= 4096 {
            let replaceable_index = self
                .updates
                .iter()
                .position(|item| classify(&item.kind) == QueueClass::Replaceable);
            if let Some(index) = replaceable_index {
                self.updates.remove(index);
                self.shed = self.shed.saturating_add(1);
            } else if incoming_class == QueueClass::Replaceable {
                self.shed = self.shed.saturating_add(1);
                return Ok(());
            } else {
                return Err(QueueFull(Box::new(update)));
            }
        }
        self.updates.push_back(update);
        Ok(())
    }

    /// Enqueues an input event, dropping the oldest input at the queue cap.
    pub fn push_input(&mut self, input: Vec<u8>) {
        if self.inputs.len() == 256 {
            self.inputs.pop_front();
            self.shed = self.shed.saturating_add(1);
        }
        self.inputs.push_back(input);
    }

    /// Drains queued updates in arrival order.
    pub fn take_updates(&mut self) -> Vec<Update> {
        self.updates.drain(..).collect()
    }

    /// Drains queued input in arrival order.
    pub fn take_inputs(&mut self) -> Vec<Vec<u8>> {
        self.inputs.drain(..).collect()
    }

    /// Returns and resets the notice count for shed updates and inputs.
    pub fn take_shed_count(&mut self) -> u64 {
        std::mem::take(&mut self.shed)
    }
}

fn classify(kind: &UpdateKind) -> QueueClass {
    match kind {
        UpdateKind::Delta { .. } | UpdateKind::ToolProgress { .. } => QueueClass::Replaceable,
        _ => QueueClass::Lossless,
    }
}

fn coalesce_update(queue: &mut VecDeque<Update>, incoming: &Update) -> bool {
    let UpdateKind::Delta {
        turn,
        channel,
        text,
    } = &incoming.kind
    else {
        if let UpdateKind::ToolProgress { call, tail } = &incoming.kind
            && let Some(existing) = queue.iter_mut().rev().find(|update| {
                matches!(&update.kind, UpdateKind::ToolProgress { call: existing_call, .. } if existing_call == call)
            })
                && let UpdateKind::ToolProgress { tail: existing_tail, .. } = &mut existing.kind {
                    *existing_tail = tail.clone();
                    existing.r#gen = incoming.r#gen;
                    existing.seq = incoming.seq;
                    return true;
                }
        return false;
    };

    if let Some(existing) = queue.back_mut()
        && let UpdateKind::Delta {
            turn: existing_turn,
            channel: existing_channel,
            text: existing_text,
        } = &mut existing.kind
        && existing_turn == turn
        && existing_channel == channel
    {
        let mut merged = String::with_capacity(existing_text.len() + text.len());
        merged.push_str(existing_text);
        merged.push_str(text);
        *existing_text = merged.into_boxed_str();
        existing.r#gen = incoming.r#gen;
        existing.seq = incoming.seq;
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{Coalescer, RegionBudget, RegionRequest};

    #[test]
    fn small_height_keeps_status_and_warning_rows() {
        let budget = RegionBudget::allocate(80, 7, RegionRequest::default());
        assert_eq!(
            (budget.status, budget.notices, budget.warning),
            (1, 1, Some(super::NarrowWarning::Rows))
        );
    }

    #[test]
    fn narrow_width_keeps_a_clipped_warning_and_status() {
        let budget = RegionBudget::allocate(11, 24, RegionRequest::default());
        assert_eq!(
            (budget.status, budget.notices, budget.warning),
            (1, 1, Some(super::NarrowWarning::Columns))
        );
    }

    #[test]
    fn overlay_keeps_its_three_row_action_floor() {
        let budget = RegionBudget::allocate(
            80,
            8,
            RegionRequest {
                overlay: Some(8),
                ..RegionRequest::default()
            },
        );
        assert_eq!((budget.overlay, budget.status), (3, 1));
    }

    #[test]
    fn queue_shed_counter_resets_after_notice() {
        let mut queue = Coalescer::default();
        for _ in 0..257 {
            queue.push_input(vec![0x61]);
        }
        assert_eq!(queue.take_inputs().len(), 256);
        assert_eq!(queue.take_shed_count(), 1);
        assert_eq!(queue.take_shed_count(), 0);
    }
}
