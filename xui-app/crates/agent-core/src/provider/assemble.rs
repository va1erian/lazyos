//! Joins streamed tool-call fragments by `index` (docs/ai-agent-plan.md
//! §5.1): the first fragment of a call carries its `id` and `name`, the rest
//! carry slices of the `arguments` text.

use crate::message::ToolCall;

#[derive(Default)]
pub struct ToolCallAssembler {
    calls: Vec<Slot>,
}

#[derive(Default)]
struct Slot {
    index: usize,
    id: String,
    name: String,
    arguments: String,
}

/// More calls than this in one answer is a model gone wrong, not a plan.
const MAX_CALLS: usize = 32;
/// The most argument text kept per call.
const MAX_ARGUMENTS: usize = 256 * 1024;

impl ToolCallAssembler {
    pub fn add(&mut self, index: usize, id: Option<&str>, name: Option<&str>, arguments: &str) {
        let slot = match self.calls.iter().position(|s| s.index == index) {
            Some(i) => &mut self.calls[i],
            None => {
                if self.calls.len() >= MAX_CALLS {
                    return;
                }
                self.calls.push(Slot {
                    index,
                    ..Slot::default()
                });
                self.calls.last_mut().expect("just pushed")
            }
        };
        if let Some(id) = id.filter(|s| !s.is_empty()) {
            slot.id = id.to_string();
        }
        if let Some(name) = name.filter(|s| !s.is_empty()) {
            slot.name = name.to_string();
        }
        let room = MAX_ARGUMENTS.saturating_sub(slot.arguments.len());
        let take = floor_boundary(arguments, room);
        slot.arguments.push_str(&arguments[..take]);
    }

    /// The calls in index order. A call with no name never started and is
    /// dropped; one with no id gets `call_<index>`.
    pub fn finish(mut self) -> Vec<ToolCall> {
        self.calls.sort_by_key(|s| s.index);
        self.calls
            .into_iter()
            .filter(|s| !s.name.is_empty())
            .map(|s| ToolCall {
                id: if s.id.is_empty() {
                    format!("call_{}", s.index)
                } else {
                    s.id
                },
                name: s.name,
                arguments: s.arguments,
            })
            .collect()
    }
}

/// The largest index at most `max` that is a char boundary of `s`.
fn floor_boundary(s: &str, max: usize) -> usize {
    let mut i = max.min(s.len());
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fragments_join_by_index_in_order() {
        let mut a = ToolCallAssembler::default();
        a.add(0, Some("call_a"), Some("get_service"), "");
        a.add(1, Some("call_b"), Some("kernel_log"), "{\"count\":");
        a.add(0, None, None, "{\"name\":\"net");
        a.add(1, None, None, "20}");
        a.add(0, None, None, "d\"}");
        let calls = a.finish();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "get_service");
        assert_eq!(calls[0].arguments, "{\"name\":\"netd\"}");
        assert_eq!(calls[1].arguments, "{\"count\":20}");
        assert!(calls[0].parsed_arguments().is_ok());
    }

    #[test]
    fn nameless_calls_are_dropped_and_missing_ids_filled() {
        let mut a = ToolCallAssembler::default();
        a.add(0, None, None, "{}");
        a.add(2, None, Some("x"), "{}");
        let calls = a.finish();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_2");
    }

    #[test]
    fn runaway_calls_and_arguments_are_capped() {
        let mut a = ToolCallAssembler::default();
        for i in 0..100 {
            a.add(i, Some("i"), Some("t"), "");
        }
        let big = "é".repeat(200_000);
        a.add(0, None, None, &big);
        a.add(0, None, None, &big);
        let calls = a.finish();
        assert_eq!(calls.len(), MAX_CALLS);
        assert!(calls[0].arguments.len() <= MAX_ARGUMENTS);
    }
}
