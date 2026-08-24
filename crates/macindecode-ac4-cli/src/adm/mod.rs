//! AC-4 OAMD 到 ADM 试听探针的直接导出。

use crate::wire::CliError;
#[cfg(not(feature = "audio-decode"))]
use crate::wire::DiagnosticCode;
use crate::{ExportAdmBwfArgs, ExportFullAdmBwfArgs};

const COMMAND: &str = "export-adm-bwf";
const FULL_COMMAND: &str = "export-full-adm-bwf";

#[cfg(not(feature = "audio-decode"))]
pub(crate) fn run(_args: ExportAdmBwfArgs) -> Result<String, CliError> {
    Err(CliError::new(
        COMMAND,
        DiagnosticCode::FeatureRequired,
        "export-adm-bwf requires rebuilding macinac4 with --features audio-decode",
    ))
}

#[cfg(not(feature = "audio-decode"))]
pub(crate) fn run_full(_args: ExportFullAdmBwfArgs) -> Result<String, CliError> {
    Err(CliError::new(
        FULL_COMMAND,
        DiagnosticCode::FeatureRequired,
        "export-full-adm-bwf requires rebuilding macinac4 with --features audio-decode",
    ))
}

#[cfg(feature = "audio-decode")]
mod enabled;

#[cfg(feature = "audio-decode")]
pub(crate) fn run(args: ExportAdmBwfArgs) -> Result<String, CliError> {
    enabled::run(args)
}

#[cfg(feature = "audio-decode")]
pub(crate) fn run_full(args: ExportFullAdmBwfArgs) -> Result<String, CliError> {
    enabled::run_full(args)
}
