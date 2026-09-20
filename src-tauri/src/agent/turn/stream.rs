//! Streaming one response into frames and assembled calls (PLAN 4.2).

use super::*;

impl Turn<'_> {
    /// Consumes one response, coalescing text and assembling tool calls.
    /// `biased`, so a cancel is polled before buffered events.
    pub(super) async fn consume(
        &self,
        plan: &TurnPlan,
        mut stream: mpsc::Receiver<ModelEvent>,
        cancel: &CancellationToken,
        seq: &mut u32,
    ) -> Streamed {
        let mut text = String::new();
        let mut frame = String::new();
        let mut assembler = crate::agent::wire::ToolCallAssembler::default();
        let mut deadline: Option<Instant> = None;
        // A call being written, reported by size only.
        let mut drafting: Option<Drafting> = None;

        let mut reason = None;
        let mut failure = None;

        loop {
            // With no open frame there is nothing to flush, so the timer must
            // never fire; `pending` is the future that never completes.
            let tick = async {
                match deadline {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending::<()>().await,
                }
            };

            tokio::select! {
                biased;

                () = cancel.cancelled() => {
                    self.flush(plan, &mut frame, drafting.as_mut(), seq);
                    tracing::debug!(turn_id = %plan.turn_id, "cancelled mid-stream");
                    return Streamed::Cancelled { text };
                }

                () = tick => {
                    self.flush(plan, &mut frame, drafting.as_mut(), seq);
                    deadline = None;
                }

                event = stream.recv() => {
                    let Some(event) = event else { break };

                    match event {
                        ModelEvent::TextDelta { text: delta } => {
                            text.push_str(&delta);
                            frame.push_str(&delta);
                            // The first token of a frame opens the window; the
                            // rest ride along inside it.
                            if deadline.is_none() {
                                deadline = Some(Instant::now() + DELTA_FRAME);
                            }
                        }
                        ModelEvent::ToolCallDelta {
                            index,
                            id,
                            name,
                            args_delta,
                            thought_signature,
                        } => {
                            // Measured before `push` takes the fragment.
                            let grown = args_delta.len() as u64;
                            let draft = drafting.get_or_insert_with(|| Drafting::new(index));
                            if draft.index != index {
                                // A second call: flush the first one's size.
                                self.emit_drafting(plan, draft, seq);
                                *draft = Drafting::new(index);
                            }
                            if let Some(name) = &name {
                                draft.tool = Some(name.clone());
                            }
                            draft.bytes = draft.bytes.saturating_add(grown);

                            assembler.push_signed(
                                index,
                                id,
                                name,
                                &args_delta,
                                thought_signature,
                            );

                            // Arguments with no text still open a frame.
                            if deadline.is_none() {
                                deadline = Some(Instant::now() + DELTA_FRAME);
                            }
                        }
                        ModelEvent::Finish { reason: stop, usage } => {
                            reason = Some((stop, usage));
                            break;
                        }
                        ModelEvent::Error { code, message, retryable } => {
                            failure = Some((code, message, retryable));
                            break;
                        }
                    }
                }
            }
        }

        self.flush(plan, &mut frame, drafting.as_mut(), seq);

        if let Some((code, message, retryable)) = failure {
            return Streamed::Failed {
                text,
                code,
                message,
                retryable,
            };
        }

        let Some((reason, usage)) = reason else {
            // Closed without `Finish`: a truncated reply, not a short success.
            return Streamed::Failed {
                text,
                code: ErrorCode::ProviderParse.as_str().to_owned(),
                message: "the provider closed the stream without finishing the reply".to_owned(),
                retryable: true,
            };
        };

        let calls = assembler.finish();
        let reason = if calls.is_empty() {
            reason
        } else {
            // Some servers report `stop` even when they streamed tool calls.
            // What arrived decides, not what was claimed.
            StopReason::ToolCalls
        };

        Streamed::Completed {
            text,
            calls,
            reason,
            usage,
        }
    }

    /// Emits the frame's text and the drafting call's size together, and
    /// empties the frame.
    pub(super) fn flush(
        &self,
        plan: &TurnPlan,
        frame: &mut String,
        drafting: Option<&mut Drafting>,
        seq: &mut u32,
    ) {
        if !frame.is_empty() {
            self.sink.emit(Event::TurnDelta(TurnDelta {
                session_id: plan.session_id.clone(),
                turn_id: plan.turn_id.clone(),
                seq: *seq,
                text: std::mem::take(frame),
            }));
            *seq = seq.saturating_add(1);
        }

        if let Some(draft) = drafting {
            self.emit_drafting(plan, draft, seq);
        }
    }

    /// Reports how far a call's arguments have got, only when that changed.
    pub(super) fn emit_drafting(&self, plan: &TurnPlan, draft: &mut Drafting, seq: &mut u32) {
        if draft.bytes == draft.reported {
            return;
        }

        self.sink.emit(Event::ToolDrafting(ToolDrafting {
            session_id: plan.session_id.clone(),
            turn_id: plan.turn_id.clone(),
            index: draft.index,
            tool: draft.tool.clone(),
            seq: *seq,
            bytes: draft.bytes,
        }));
        draft.reported = draft.bytes;
        *seq = seq.saturating_add(1);
    }
}
