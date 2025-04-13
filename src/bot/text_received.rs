use super::{DialogueState, Error, HandleMessage, HandlerResult};

impl HandleMessage<'_> {
    pub(crate) async fn handle_text(self, text: &str) -> HandlerResult {
        self.with_dialogue(async move |dialogue| {
            match dialogue.state {
                DialogueState::ReceiveTag {
                    previous_conditions,
                } => todo!(),
                DialogueState::ReceiveNegation {
                    previous_conditions,
                    tag,
                } => todo!(),
                DialogueState::ReceivePattern {
                    previous_conditions,
                    tag,
                    negation,
                } => todo!(),
                DialogueState::DeleteFilter => todo!(),
                _ => return Err(Error::UnexpectedMessage),
            };

            Ok(dialogue)
        })
        .await
    }
}
