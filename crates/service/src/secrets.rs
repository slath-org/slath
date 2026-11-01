// SPDX-FileCopyrightText: 2026 Erich Seifert <dev@erichseifert.de>
// SPDX-License-Identifier: AGPL-3.0-or-later
//! Secrets at rest: sealing and opening values with the instance's
//! envelope, and master key rotation.

use slath_auth::crypto::{CryptoError, Location, Sealed};
use slath_core::Secret;
use slath_db::secrets::{ENCRYPTED_COLUMNS, EncryptedColumn};

use crate::Services;
use crate::error::{Result, ServiceError};

/// Progress of a rotation, per encrypted column.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct RotationReport {
    pub current_key: String,
    /// Rows re-encrypted with the current key by this run.
    pub reencrypted: u64,
    /// Rows that need an old key that isn't loaded, or don't decrypt.
    pub failed: Vec<RotationFailure>,
    /// Keys no row uses any more, marked retired by this run.
    pub retired: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RotationFailure {
    pub table: &'static str,
    pub row: String,
    pub key_id: String,
    pub error: String,
}

impl Services {
    /// Encrypts `plain` for `table.column` of row `row` with the current
    /// key, recording the key first.
    pub async fn seal(&self, plain: &[u8], table: &str, column: &str, row: &str) -> Result<Sealed> {
        slath_db::secrets::register_key(&self.pool, self.envelope.current_id()).await?;
        self.envelope
            .seal(plain, Location { table, column, row })
            .map_err(ServiceError::internal)
    }

    /// Decrypts a value read from `table.column` of row `row`.
    pub fn open(
        &self,
        ciphertext: &[u8],
        key_id: &str,
        table: &str,
        column: &str,
        row: &str,
    ) -> Result<Secret<Vec<u8>>> {
        self.envelope
            .open(ciphertext, key_id, Location { table, column, row })
            .map_err(ServiceError::internal)
    }

    /// Re-encrypts every value not yet under the current master key, then
    /// retires keys no value uses. Safe to interrupt and re-run: each row
    /// is replaced on its own (compare and swap), and rows already under the
    /// current key are skipped.
    pub async fn rotate_keys(&self, batch: i64) -> Result<RotationReport> {
        let current = self.envelope.current_id().to_owned();
        slath_db::secrets::register_key(&self.pool, &current).await?;
        let mut report = RotationReport {
            current_key: current.clone(),
            ..RotationReport::default()
        };
        for col in ENCRYPTED_COLUMNS {
            self.rotate_column(*col, batch.max(1), &mut report).await?;
        }
        if report.failed.is_empty() {
            report.retired = slath_db::secrets::retire_unused(&self.pool, &current).await?;
        }
        Ok(report)
    }

    async fn rotate_column(
        &self,
        col: EncryptedColumn,
        batch: i64,
        report: &mut RotationReport,
    ) -> Result<()> {
        let current = self.envelope.current_id().to_owned();
        let mut after: Option<String> = None;
        loop {
            let rows = slath_db::secrets::rows_not_using(
                &self.pool,
                col,
                &current,
                after.as_deref(),
                batch,
            )
            .await?;
            let Some(last) = rows.last() else {
                return Ok(());
            };
            after = Some(last.id.clone());
            for row in &rows {
                let at = Location {
                    table: col.table,
                    column: col.value,
                    row: &row.id,
                };
                match self.envelope.reseal(&row.value, &row.key_id, at) {
                    Ok(Some(sealed)) => {
                        if slath_db::secrets::replace(
                            &self.pool,
                            col,
                            row,
                            &sealed.ciphertext,
                            &sealed.key_id,
                        )
                        .await?
                        {
                            report.reencrypted += 1;
                        }
                    }
                    Ok(None) => {}
                    Err(e @ (CryptoError::UnknownKey(_) | CryptoError::Decrypt)) => {
                        report.failed.push(RotationFailure {
                            table: col.table,
                            row: row.id.clone(),
                            key_id: row.key_id.clone(),
                            error: e.to_string(),
                        });
                    }
                    Err(e) => return Err(ServiceError::internal(e)),
                }
            }
        }
    }
}
