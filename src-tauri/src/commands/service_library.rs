use super::CmdResult;
use crate::ui_message::{app, UiMessage};
use bazza_service_client::{
    api::{
        desktop::{MAX_DESKTOP_DATA_BYTES, MAX_DESKTOP_ITEMS},
        library::{DesktopLibraryItem, DesktopLibraryRequest},
        rpc::ErrorCode,
    },
    ClientError, RepositoryClient,
};
use bazza_store::{Folder, Tag};
use serde::Serialize;
use tauri::State;

const MAX_ORGANIZATION_PAGES: usize = 128;

#[derive(Serialize)]
struct ServiceFolder {
    #[serde(flatten)]
    folder: Folder,
    revision: String,
}

#[derive(Default, Serialize)]
pub(crate) struct ServiceLibraryOrganization {
    folders: Vec<ServiceFolder>,
    tags: Vec<Tag>,
}

fn refresh_required() -> UiMessage {
    app(
        "native.serviceOrganizationRefreshRequired",
        "The library changed. Refresh it before trying again.",
    )
}

fn invalid_request() -> UiMessage {
    app(
        "native.serviceOrganizationRequestInvalid",
        "The service rejected this library request.",
    )
}

fn service_error(error: ClientError) -> UiMessage {
    if let ClientError::Remote(error) = error {
        match error.code {
            ErrorCode::Conflict | ErrorCode::ResyncRequired | ErrorCode::NotFound => {
                return refresh_required()
            }
            ErrorCode::InvalidArgument | ErrorCode::IdempotencyConflict => {
                return invalid_request()
            }
            _ => {}
        }
    }
    app(
        "native.serviceOrganizationUnavailable",
        "The library service could not complete the request.",
    )
}

#[tauri::command]
pub(crate) async fn get_service_library_organization(
    repository: State<'_, RepositoryClient>,
) -> CmdResult<ServiceLibraryOrganization> {
    let identity = repository.identity().await;
    let mut organization = ServiceLibraryOrganization::default();
    let mut continuation = None;
    let mut sequence = None;
    for _ in 0..MAX_ORGANIZATION_PAGES {
        let page = repository
            .library_page(continuation, MAX_DESKTOP_ITEMS, MAX_DESKTOP_DATA_BYTES)
            .await
            .map_err(service_error)?;
        if page.identity != identity
            || sequence.is_some_and(|value| value != page.snapshot_sequence)
        {
            return Err(refresh_required());
        }
        sequence = Some(page.snapshot_sequence);
        for item in page.items {
            match item {
                DesktopLibraryItem::Folder {
                    id,
                    name,
                    parent_id,
                    default_template_id,
                    meeting_count,
                    revision,
                } => {
                    organization.folders.push(ServiceFolder {
                        folder: Folder {
                            id,
                            name,
                            parent_id,
                            default_template_id,
                            meeting_count,
                        },
                        revision,
                    });
                }
                DesktopLibraryItem::Tag {
                    id,
                    name,
                    meeting_count,
                    ..
                } => {
                    organization.tags.push(Tag {
                        id,
                        name,
                        meeting_count,
                    });
                }
            }
        }
        continuation = page.next;
        if continuation.is_none() {
            return Ok(organization);
        }
    }
    Err(app(
        "native.serviceOrganizationTooLarge",
        "The library has too many folders and tags for this window.",
    ))
}

#[tauri::command]
pub(crate) async fn mutate_service_library_organization(
    repository: State<'_, RepositoryClient>,
    request: DesktopLibraryRequest,
) -> CmdResult<()> {
    match request {
        DesktopLibraryRequest::FolderCreate { .. }
        | DesktopLibraryRequest::FolderRename { .. }
        | DesktopLibraryRequest::FolderDelete { .. }
        | DesktopLibraryRequest::MeetingMove { .. } => {}
        _ => return Err(invalid_request()),
    }
    repository
        .library_mutate(request)
        .await
        .map_err(service_error)?;
    Ok(())
}
