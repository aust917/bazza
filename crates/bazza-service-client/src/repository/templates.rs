//! Epoch/scope-bound template continuations. No full-body autocollection.
use super::{client, resync, snapshot, RepositoryClient, RepositoryIdentity, RepositorySnapshot};
use crate::ClientError;
use bazza_service_api::templates::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateListContinuation {
    pub library_id: Uuid,
    pub library_epoch: Uuid,
    pub cursor: TemplateListCursor,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TemplateFieldContinuation {
    pub library_id: Uuid,
    pub library_epoch: Uuid,
    pub cursor: TemplateReadCursor,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryTemplateListPage {
    pub identity: RepositoryIdentity,
    pub catalog_revision: String,
    pub items: Vec<TemplateHeader>,
    pub next: Option<TemplateListContinuation>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryTemplateReadRequest {
    pub template_id: String,
    pub expected_revision: String,
    pub field: TemplateField,
    pub cursor: Option<TemplateFieldContinuation>,
    pub max_bytes: u32,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepositoryTemplateFieldPage {
    pub identity: RepositoryIdentity,
    pub header: TemplateHeader,
    pub field: TemplateField,
    pub text: String,
    pub byte_offset: u64,
    pub total_bytes: u64,
    pub next: Option<TemplateFieldContinuation>,
}
impl std::fmt::Debug for RepositoryTemplateFieldPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepositoryTemplateFieldPage")
            .field("field", &self.field)
            .field("byte_offset", &self.byte_offset)
            .field("fragment_bytes", &self.text.len())
            .finish_non_exhaustive()
    }
}
impl RepositoryClient {
    pub async fn templates_list(
        &self,
        cursor: Option<TemplateListContinuation>,
        limit: u16,
        max_bytes: u32,
    ) -> Result<RepositoryTemplateListPage, ClientError> {
        let mut state = self.shared.state.lock().await;
        if cursor.as_ref().is_some_and(|c| {
            c.library_id != state.identity.library_id
                || c.library_epoch != state.identity.library_epoch
                || c.cursor.library_scope != self.shared.cursor_scope
        }) {
            return Err(resync());
        }
        let reply = client(&mut state)?
            .desktop_templates(DesktopTemplatesRequest::List(TemplateListRequest {
                cursor: cursor.map(|c| c.cursor),
                limit,
                max_bytes,
            }))
            .await?
            .data;
        let DesktopTemplatesReply::List(page) = reply else {
            state.client.take();
            return Err(ClientError::Protocol);
        };
        if page.library_scope != self.shared.cursor_scope {
            state.client.take();
            return Err(resync());
        }
        Ok(RepositoryTemplateListPage {
            identity: state.identity.clone(),
            catalog_revision: page.catalog_revision,
            items: page.items,
            next: page.next.map(|cursor| TemplateListContinuation {
                library_id: state.identity.library_id,
                library_epoch: state.identity.library_epoch,
                cursor,
            }),
        })
    }
    pub async fn template_field(
        &self,
        r: RepositoryTemplateReadRequest,
    ) -> Result<RepositoryTemplateFieldPage, ClientError> {
        let mut state = self.shared.state.lock().await;
        if r.cursor.as_ref().is_some_and(|c| {
            c.library_id != state.identity.library_id
                || c.library_epoch != state.identity.library_epoch
                || c.cursor.library_scope != self.shared.cursor_scope
        }) {
            return Err(resync());
        }
        let reply = client(&mut state)?
            .desktop_templates(DesktopTemplatesRequest::ReadField(TemplateReadRequest {
                template_id: r.template_id,
                expected_revision: r.expected_revision,
                field: r.field,
                cursor: r.cursor.map(|c| c.cursor),
                max_bytes: r.max_bytes,
            }))
            .await?
            .data;
        let DesktopTemplatesReply::ReadField(page) = reply else {
            state.client.take();
            return Err(ClientError::Protocol);
        };
        if page.library_scope != self.shared.cursor_scope {
            state.client.take();
            return Err(resync());
        }
        Ok(RepositoryTemplateFieldPage {
            identity: state.identity.clone(),
            header: page.header,
            field: page.field,
            text: page.text,
            byte_offset: page.byte_offset,
            total_bytes: page.total_bytes,
            next: page.next.map(|cursor| TemplateFieldContinuation {
                library_id: state.identity.library_id,
                library_epoch: state.identity.library_epoch,
                cursor,
            }),
        })
    }
    async fn template_mutation(
        &self,
        request: DesktopTemplatesRequest,
    ) -> Result<RepositorySnapshot<DesktopTemplatesReply>, ClientError> {
        let mut state = self.shared.state.lock().await;
        let data = client(&mut state)?.desktop_templates(request).await?.data;
        Ok(snapshot(&state, data))
    }
    pub async fn template_describe(
        &self,
        id: String,
    ) -> Result<RepositorySnapshot<DesktopTemplatesReply>, ClientError> {
        self.template_mutation(DesktopTemplatesRequest::Describe { template_id: id })
            .await
    }
    pub async fn template_write_begin(
        &self,
        r: TemplateBeginWrite,
    ) -> Result<RepositorySnapshot<DesktopTemplatesReply>, ClientError> {
        self.template_mutation(DesktopTemplatesRequest::BeginWrite(r))
            .await
    }
    pub async fn template_write_append(
        &self,
        r: TemplateAppendWrite,
    ) -> Result<RepositorySnapshot<DesktopTemplatesReply>, ClientError> {
        self.template_mutation(DesktopTemplatesRequest::AppendWrite(r))
            .await
    }
    pub async fn template_write_commit(
        &self,
        r: TemplateUploadRequest,
    ) -> Result<RepositorySnapshot<DesktopTemplatesReply>, ClientError> {
        self.template_mutation(DesktopTemplatesRequest::CommitWrite(r))
            .await
    }
    pub async fn template_write_abort(
        &self,
        r: TemplateUploadRequest,
    ) -> Result<RepositorySnapshot<DesktopTemplatesReply>, ClientError> {
        self.template_mutation(DesktopTemplatesRequest::AbortWrite(r))
            .await
    }
    pub async fn template_delete(
        &self,
        r: TemplateDeleteRequest,
    ) -> Result<RepositorySnapshot<DesktopTemplatesReply>, ClientError> {
        self.template_mutation(DesktopTemplatesRequest::Delete(r))
            .await
    }
    pub async fn templates_seed_missing(
        &self,
        r: TemplateSeedRequest,
    ) -> Result<RepositorySnapshot<DesktopTemplatesReply>, ClientError> {
        self.template_mutation(DesktopTemplatesRequest::SeedMissingBuiltins(r))
            .await
    }
}
