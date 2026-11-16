//! The engine layer as the app hosts it: a main and a side channel, each with its own warm-session
//! pool and coordinator, one tool service for both, and the commands the app sends it.

use std::sync::Arc;

use futures::future::join_all;

use crate::coordinator::{
    AgentEvent, CoordinatorOptions, Emitter, ReloadStatus, RunCoordinator, SettingsReloadEvent,
    StartRun,
};
use crate::engines::EngineRouter;
use crate::mcp_http::McpHttpHost;
use crate::session_pool::SessionPool;
use crate::tools::ToolHost;
use crate::types::{Continuation, CoordinationRole, RunChannel};

/// Everything the app says to the engine layer about runs.
pub enum RunCommand {
    ReloadSettings,
    Start(Box<StartRun>),
    Cancel {
        task_id: String,
        run_id: String,
    },
    AnswerQuestion {
        task_id: String,
        run_id: String,
        request_id: String,
        question_id: String,
        text: String,
    },
    Steer {
        task_id: String,
        run_id: String,
        message_id: String,
        prompt: String,
    },
    StopProcess {
        task_id: String,
        process_id: String,
    },
    Label {
        task_id: String,
        title: String,
    },
    Archive {
        task_id: String,
        channel: RunChannel,
        continuation: Option<Continuation>,
        archived: bool,
    },
    Role {
        task_id: String,
        coordination_role: Option<CoordinationRole>,
    },
    Approval {
        task_id: String,
        run_id: String,
        approval_id: String,
        allow: bool,
    },
}

pub struct AgentWorker {
    main: RunCoordinator,
    side: RunCoordinator,
    pools: [SessionPool; 2],
    host: McpHttpHost,
    emit: Emitter,
}

impl AgentWorker {
    /// `generated_images` is where engines save images they generate. Must be created inside a Tokio runtime.
    pub fn new(
        emit: Emitter,
        options: CoordinatorOptions,
        generated_images: Option<String>,
    ) -> Self {
        let host = McpHttpHost::new();
        let tool_host: Arc<dyn ToolHost> = Arc::new(host.clone());
        let pools = [SessionPool::default(), SessionPool::default()];
        let coordinator = |pool: &SessionPool| {
            RunCoordinator::new(
                Arc::new(EngineRouter::for_channel(
                    Some(tool_host.clone()),
                    pool.clone(),
                    generated_images.clone(),
                )),
                emit.clone(),
                options.clone(),
            )
        };
        AgentWorker {
            main: coordinator(&pools[0]),
            side: coordinator(&pools[1]),
            pools,
            host,
            emit,
        }
    }

    fn coordinators(&self) -> [&RunCoordinator; 2] {
        [&self.main, &self.side]
    }

    fn channel(&self, channel: RunChannel) -> &RunCoordinator {
        match channel {
            RunChannel::Main => &self.main,
            RunChannel::Side => &self.side,
        }
    }

    /// A command names a thread rather than a channel, so it is offered to each until one owns the run.
    pub fn handle(&self, command: RunCommand) {
        match command {
            RunCommand::ReloadSettings => self.reload_settings(),
            RunCommand::Start(start) => self.channel(start.channel).start(*start),
            RunCommand::Cancel { task_id, run_id } => {
                self.coordinators()
                    .iter()
                    .any(|coordinator| coordinator.cancel(&task_id, &run_id));
            }
            RunCommand::AnswerQuestion {
                task_id,
                run_id,
                request_id,
                question_id,
                text,
            } => {
                self.coordinators().iter().any(|coordinator| {
                    coordinator.answer_question(&task_id, &run_id, &request_id, &question_id, &text)
                });
            }
            RunCommand::Steer {
                task_id,
                run_id,
                message_id,
                prompt,
            } => {
                self.coordinators()
                    .iter()
                    .any(|coordinator| coordinator.steer(&task_id, &run_id, &message_id, &prompt));
            }
            RunCommand::StopProcess {
                task_id,
                process_id,
            } => {
                self.coordinators()
                    .iter()
                    .any(|coordinator| coordinator.stop_process(&task_id, &process_id));
            }
            RunCommand::Label { task_id, title } => {
                self.coordinators()
                    .iter()
                    .any(|coordinator| coordinator.label_thread(&task_id, &title));
            }
            RunCommand::Archive {
                task_id,
                channel,
                continuation,
                archived,
            } => {
                self.channel(channel)
                    .archive_thread(&task_id, continuation, archived);
            }
            RunCommand::Role {
                task_id,
                coordination_role,
            } => {
                for coordinator in self.coordinators() {
                    coordinator.retire_out_of_role(&task_id, coordination_role);
                }
            }
            RunCommand::Approval {
                task_id,
                run_id,
                approval_id,
                allow,
            } => {
                self.coordinators().iter().any(|coordinator| {
                    coordinator.decide_approval(&task_id, &run_id, &approval_id, allow)
                });
            }
        }
    }

    /// Retires every warm session once its work finishes, so the next run reads settings afresh.
    fn reload_settings(&self) {
        let waits: Vec<_> = self
            .pools
            .iter()
            .map(SessionPool::reload_settings)
            .collect();
        let emit = self.emit.clone();
        emit(AgentEvent::SettingsReload(SettingsReloadEvent::Status {
            status: ReloadStatus::Pending,
            message: None,
        }));
        tokio::spawn(async move {
            join_all(waits).await;
            emit(AgentEvent::SettingsReload(SettingsReloadEvent::Status {
                status: ReloadStatus::Reloaded,
                message: None,
            }));
        });
    }

    /// Lets a thread's session go for memory protection, in whichever channel holds it.
    pub fn terminate_session(&self, task_id: &str, session_id: &str, idle_only: bool) {
        for pool in &self.pools {
            pool.terminate(task_id, session_id, idle_only);
        }
    }

    /// Sessions outlive the runs that used them, so leaving takes them down explicitly.
    pub fn close(&self) {
        for coordinator in self.coordinators() {
            coordinator.close_all();
        }
        self.host.close();
    }
}
