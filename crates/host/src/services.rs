//! The host's long-lived services for one data folder, made once at launch.

use std::path::PathBuf;
use std::sync::Arc;

use crate::attachments::AttachmentStore;
use crate::message_images::MessageImageStore;
use crate::tools::McpHttpHost;
use crate::user_data::AppProfile;
use crate::workspaces::{WorkspaceError, WorkspaceService};
use crate::worktrees::WorktreeService;

pub struct HostServices {
    pub profile: AppProfile,
    pub workspaces: Arc<WorkspaceService>,
    pub worktrees: Arc<WorktreeService>,
    pub attachments: AttachmentStore,
    pub message_images: Arc<MessageImageStore>,
    pub tools: Arc<McpHttpHost>,
}

impl HostServices {
    /// Opens the registry and the stores under the profile's data folder.
    pub async fn open(profile: AppProfile) -> Result<Self, WorkspaceError> {
        tokio::fs::create_dir_all(&profile.user_data).await?;
        let workspaces = Arc::new(WorkspaceService::open(profile.user_data.join("workspaces.v1.json"), profile.user_data.join("projectless")).await?);
        let worktrees = Arc::new(WorktreeService::new(profile.worktrees_root.clone(), legacy_worktrees_roots(&profile), workspaces.clone()));
        Ok(Self {
            attachments: AttachmentStore::new(&profile.user_data),
            message_images: MessageImageStore::new(&profile.user_data),
            tools: Arc::new(McpHttpHost::new()),
            workspaces,
            worktrees,
            profile,
        })
    }

    /// The roots the app keeps checkouts under, which a folder the user names may not be inside.
    pub fn worktrees_roots(&self) -> Vec<PathBuf> {
        self.worktrees.owned_roots().to_vec()
    }
}

/// Where older builds kept checkouts, and where an imported profile's were made: still the app's,
/// listed and removable, never created in again.
pub fn legacy_worktrees_roots(profile: &AppProfile) -> Vec<PathBuf> {
    act_store::profile::legacy_worktrees_roots(&profile.user_data, &profile.worktrees_root)
}
