//! One closed private library call; wrong kinds/operations poison the stream.
use crate::{ClientError, ServiceClient};
use bazza_service_api::{
    desktop::DesktopEnvelope,
    library::{DesktopLibraryReply, DesktopLibraryRequest},
    rpc::{ErrorCode, Reply, Request, RpcError},
};

impl ServiceClient {
    pub async fn desktop_library(
        &mut self,
        request: DesktopLibraryRequest,
    ) -> Result<DesktopEnvelope<DesktopLibraryReply>, ClientError> {
        if !request.valid() {
            return Err(RpcError::new(ErrorCode::InvalidArgument).into());
        }
        let original = request.clone();
        match self.call(Request::DesktopLibrary(request)).await? {
            Reply::DesktopLibrary(value)
                if self.desktop_envelope_valid(&value) && value.data.valid_for(&original) =>
            {
                Ok(*value)
            }
            _ => self.invalid_worker_reply(),
        }
    }
}
