use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::{json, Map, Value};
use tokio_util::sync::CancellationToken;

use crate::provider::RunInput;
use crate::types::{AgentQuestion, QuestionOption, QuestionRequest};

use super::app_server::{AppServerClient, IncomingRequest};

/// Questions the server put to the user. Each is withdrawn when Codex resolves it or its turn ends.
#[derive(Clone, Default)]
pub struct CodexQuestions {
    pending: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl CodexQuestions {
    /// Withdraws the question the server says it no longer needs.
    pub fn resolved(&self, request_id: &Value) {
        if let Some(token) = self.pending.lock().unwrap().remove(&request_id.to_string()) {
            token.cancel();
        }
    }

    pub fn answer(&self, request: IncomingRequest, input: &RunInput) {
        let params = &request.params;
        let questions = params["questions"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|question| AgentQuestion {
                id: question["id"].as_str().unwrap_or_default().to_string(),
                header: question["header"].as_str().unwrap_or_default().to_string(),
                question: question["question"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
                options: question["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|option| QuestionOption {
                        label: option["label"].as_str().unwrap_or_default().to_string(),
                        description: option["description"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string(),
                    })
                    .collect(),
            })
            .collect();
        let token = CancellationToken::new();
        let key = request.id.to_string();
        self.pending
            .lock()
            .unwrap()
            .insert(key.clone(), token.clone());
        let ask = input.hooks.ask_question.clone();
        let pending = self.pending.clone();
        let blocking = params["isBlocking"].as_bool().unwrap_or(false);
        tokio::spawn(async move {
            let answers = ask(
                QuestionRequest {
                    questions,
                    blocking,
                },
                token.clone(),
            )
            .await;
            pending.lock().unwrap().remove(&key);
            if token.is_cancelled() {
                return;
            }
            match answers {
                None => request.fail(-32000, "The question was closed without an answer."),
                Some(answers) => {
                    let answers: Map<String, Value> = answers
                        .into_iter()
                        .map(|(id, answer)| (id, json!({ "answers": [answer] })))
                        .collect();
                    request.respond(json!({ "answers": answers }));
                }
            }
        });
    }

    /// Questions an asynchronous message asked; their answers are steered into the turn going.
    pub async fn answer_async(
        &self,
        native: &[Value],
        input: &RunInput,
        client: &AppServerClient,
        thread_id: &str,
        active_turn: impl Fn() -> Option<String>,
    ) -> anyhow::Result<()> {
        let questions: Vec<AgentQuestion> = native
            .iter()
            .enumerate()
            .map(|(index, question)| AgentQuestion {
                id: index.to_string(),
                header: "Question".into(),
                question: question["title"].as_str().unwrap_or_default().to_string(),
                options: question["options"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(|label| QuestionOption {
                        label: label.into(),
                        description: String::new(),
                    })
                    .collect(),
            })
            .collect();
        let answers = (input.hooks.ask_question)(
            QuestionRequest {
                questions: questions.clone(),
                blocking: false,
            },
            CancellationToken::new(),
        )
        .await;
        let (Some(answers), Some(turn_id)) = (answers, active_turn()) else {
            return Ok(());
        };
        let text = questions
            .iter()
            .map(|question| {
                format!(
                    "{}\n{}",
                    question.question,
                    answers.get(&question.id).cloned().unwrap_or_default()
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        client.request("turn/steer", json!({ "threadId": thread_id, "expectedTurnId": turn_id, "input": [{ "type": "text", "text": text, "text_elements": [] }] })).await?;
        Ok(())
    }

    /// The turn's questions go with it: nobody is left to answer them.
    pub fn close(&self) {
        for (_, token) in self.pending.lock().unwrap().drain() {
            token.cancel();
        }
    }
}
