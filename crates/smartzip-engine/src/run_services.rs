//! Database-backed services derived from one immutable run policy.

use crate::history::{DbKnownFileStore, DbTaskHistoryRecorder, RunStores};
use smartzip_config::{PasswordMode, PasswordSource, StateMode};
use smartzip_passwords::PasswordService;

pub struct RunServices<'a> {
    pub passwords: PasswordService<'a>,
    pub history: Option<DbTaskHistoryRecorder<'a>>,
    pub known: Option<DbKnownFileStore<'a>>,
}

impl RunServices<'_> {
    pub fn has_stores(&self) -> bool {
        self.history.is_some() || self.known.is_some()
    }

    pub fn stores(&self) -> RunStores<'_> {
        RunStores {
            history: self
                .history
                .as_ref()
                .map(|s| s as &dyn crate::history::TaskHistoryRecorder),
            known_files: self
                .known
                .as_ref()
                .map(|s| s as &dyn crate::history::KnownFileStore),
        }
    }
}

impl crate::CompiledRunPolicy {
    pub fn services<'a>(&self, connection: Option<&'a rusqlite::Connection>) -> RunServices<'a> {
        let c = self.values();
        RunServices {
            passwords: PasswordService::configured(
                connection.map(smartzip_db::password::PasswordRepository::new),
                c.passwords.clone(),
                c.state.mode,
            ),
            // Completed-file reuse still reads history when writes are disabled.
            history: connection
                .filter(|_| c.state.mode != StateMode::Off)
                .map(|connection| {
                    DbTaskHistoryRecorder::new(connection)
                        .with_writes(c.state.mode == StateMode::ReadWrite && c.state.history)
                }),
            known: connection
                .filter(|_| c.state.mode != StateMode::Off && c.state.known_files != StateMode::Off)
                .map(|connection| DbKnownFileStore {
                    connection,
                    writable: c.state.mode == StateMode::ReadWrite
                        && c.state.known_files == StateMode::ReadWrite,
                    password_hint: c.extraction.reuse.password_hint
                        && c.passwords.mode == PasswordMode::Auto
                        && c.passwords.sources.contains(&PasswordSource::Known),
                    encoding_hint: c.extraction.reuse.encoding_hint
                        && c.extraction.encoding.mode == "auto",
                }),
        }
    }
}
