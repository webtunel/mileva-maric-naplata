use std::path::Path;

use chrono::Utc;
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};

use crate::models::{KioskError, KioskResult, PrintedTicket, SaleRecord, Settings};

pub struct Db {
    conn: Mutex<Connection>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ZReport {
    pub from_ts: i64,
    pub to_ts: i64,
    pub count_sales: u32,      // paid sales only
    pub count_tickets: u32,
    pub total_rsd: i64,        // revenue from paid sales
    pub abandoned_rsd: i64,    // partial cash from abandoned sessions (for reconciliation)
    pub by_type: Vec<(String, u32, i64)>,
}

struct StoredSale {
    id: String,
    created_at: i64,
    total_rsd: i64,
    inserted_rsd: i64,
    num_tickets: i64,
    reprinted_count: i64,
    status: String,
    printed: bool,
}

fn db_error(error: impl std::fmt::Display) -> KioskError {
    KioskError::Db(error.to_string())
}

fn checked_u32(value: i64, field: &str) -> KioskResult<u32> {
    u32::try_from(value)
        .map_err(|_| KioskError::Db(format!("invalid {field} value in database: {value}")))
}

pub fn open(path: &Path) -> KioskResult<Db> {
    let conn = Connection::open(path).map_err(db_error)?;
    conn.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=FULL;
         PRAGMA foreign_keys=ON;

         CREATE TABLE IF NOT EXISTS settings(
             key TEXT PRIMARY KEY,
             value TEXT NOT NULL
         );

         CREATE TABLE IF NOT EXISTS secrets(
             name TEXT PRIMARY KEY,
             value BLOB NOT NULL
         );

         CREATE TABLE IF NOT EXISTS sales(
             id TEXT PRIMARY KEY,
             created_at INTEGER,
             total_rsd INTEGER,
             inserted_rsd INTEGER,
             num_tickets INTEGER,
             printed INTEGER NOT NULL DEFAULT 0,
             reprinted_count INTEGER NOT NULL DEFAULT 0,
             status TEXT NOT NULL DEFAULT 'pending'
         );

         CREATE TABLE IF NOT EXISTS tickets(
             id TEXT PRIMARY KEY,
             sale_id TEXT REFERENCES sales(id),
             type_code TEXT,
             label TEXT,
             price_rsd INTEGER,
             issued_at INTEGER,
             qr_token TEXT,
             printed_at INTEGER,
             redeemed_at INTEGER
         );

         CREATE INDEX IF NOT EXISTS idx_sales_created_at ON sales(created_at);
         CREATE INDEX IF NOT EXISTS idx_sales_status ON sales(status);
         CREATE INDEX IF NOT EXISTS idx_tickets_sale_id ON tickets(sale_id);",
    )
    .map_err(db_error)?;

    // Migration for databases created before the `status` column existed. SQLite has no
    // ADD COLUMN IF NOT EXISTS, so ignore the "duplicate column" error on a fresh schema.
    if let Err(e) = conn.execute(
        "ALTER TABLE sales ADD COLUMN status TEXT NOT NULL DEFAULT 'pending'",
        [],
    ) {
        let msg = e.to_string();
        if !msg.contains("duplicate column") {
            return Err(db_error(e));
        }
    }

    Ok(Db {
        conn: Mutex::new(conn),
    })
}

impl Db {
    pub fn load_settings(&self) -> KioskResult<Settings> {
        let conn = self.conn.lock();
        let value: Option<String> = conn
            .query_row(
                "SELECT value FROM settings WHERE key = ?1",
                params!["settings"],
                |row| row.get(0),
            )
            .optional()
            .map_err(db_error)?;

        // Never let a malformed/older settings row brick the kiosk: fall back to defaults.
        Ok(value
            .and_then(|json| serde_json::from_str(&json).ok())
            .unwrap_or_default())
    }

    pub fn save_settings(&self, s: &Settings) -> KioskResult<()> {
        let value = serde_json::to_string(s).map_err(db_error)?;
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO settings(key, value) VALUES (?1, ?2)",
            params!["settings", value],
        )
        .map_err(db_error)?;
        Ok(())
    }

    pub fn get_secret(&self, name: &str) -> KioskResult<Option<Vec<u8>>> {
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT value FROM secrets WHERE name = ?1",
            params![name],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_error)
    }

    pub fn set_secret(&self, name: &str, val: &[u8]) -> KioskResult<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT OR REPLACE INTO secrets(name, value) VALUES (?1, ?2)",
            params![name, val],
        )
        .map_err(db_error)?;
        Ok(())
    }

    /// Insert a `pending` sale BEFORE cash acceptance begins. No tickets yet — they are
    /// minted only once the sale is fully paid. This is the money-safety anchor: a row
    /// exists to attach every inserted note to, even if power is lost mid-session.
    pub fn create_pending_sale(
        &self,
        sale_id: &str,
        created_at: i64,
        total_rsd: i64,
        num_tickets: u32,
    ) -> KioskResult<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO sales(id, created_at, total_rsd, inserted_rsd, num_tickets,
                               printed, reprinted_count, status)
             VALUES (?1, ?2, ?3, 0, ?4, 0, 0, 'pending')",
            params![sale_id, created_at, total_rsd, i64::from(num_tickets)],
        )
        .map_err(db_error)?;
        Ok(())
    }

    /// Persist the running inserted total on every credit, so cash in the box is never
    /// only in memory.
    pub fn update_inserted(&self, sale_id: &str, inserted_rsd: i64) -> KioskResult<()> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE sales SET inserted_rsd = ?1 WHERE id = ?2",
            params![inserted_rsd, sale_id],
        )
        .map_err(db_error)?;
        Ok(())
    }

    /// Mark a pending sale `paid` and insert its tickets, atomically.
    pub fn finalize_paid(
        &self,
        sale_id: &str,
        inserted_rsd: i64,
        tickets: &[PrintedTicket],
    ) -> KioskResult<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(db_error)?;

        let updated = tx
            .execute(
                "UPDATE sales SET status = 'paid', inserted_rsd = ?1 WHERE id = ?2",
                params![inserted_rsd, sale_id],
            )
            .map_err(db_error)?;
        if updated == 0 {
            return Err(KioskError::Db(format!(
                "prodaja za finalizaciju nije pronađena: {sale_id}"
            )));
        }

        {
            let mut stmt = tx
                .prepare(
                    "INSERT INTO tickets(
                         id, sale_id, type_code, label, price_rsd, issued_at, qr_token,
                         printed_at, redeemed_at
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL, NULL)",
                )
                .map_err(db_error)?;

            for ticket in tickets {
                stmt.execute(params![
                    ticket.id,
                    sale_id,
                    ticket.type_code,
                    ticket.label,
                    ticket.price_rsd,
                    ticket.issued_at,
                    ticket.qr_token,
                ])
                .map_err(db_error)?;
            }
        }

        tx.commit().map_err(db_error)?;
        Ok(())
    }

    /// Mark a pending sale `abandoned` with the partial cash actually taken. The money is
    /// in the box; this row is what staff use to reconcile / refund. No tickets are issued.
    pub fn mark_abandoned(&self, sale_id: &str, inserted_rsd: i64) -> KioskResult<()> {
        let conn = self.conn.lock();
        conn.execute(
            "UPDATE sales SET status = 'abandoned', inserted_rsd = ?1 WHERE id = ?2",
            params![inserted_rsd, sale_id],
        )
        .map_err(db_error)?;
        Ok(())
    }

    /// Remove a pending sale that took zero cash (clean cancel).
    pub fn delete_sale(&self, sale_id: &str) -> KioskResult<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM sales WHERE id = ?1", params![sale_id])
            .map_err(db_error)?;
        Ok(())
    }

    pub fn mark_printed(&self, sale_id: &str) -> KioskResult<()> {
        let now = Utc::now().timestamp();
        let mut conn = self.conn.lock();
        let tx = conn.transaction().map_err(db_error)?;
        let updated = tx
            .execute("UPDATE sales SET printed = 1 WHERE id = ?1", params![sale_id])
            .map_err(db_error)?;
        if updated == 0 {
            return Err(KioskError::Db(format!("prodaja nije pronađena: {sale_id}")));
        }
        tx.execute(
            "UPDATE tickets
             SET printed_at = ?1
             WHERE sale_id = ?2 AND printed_at IS NULL",
            params![now, sale_id],
        )
        .map_err(db_error)?;
        tx.commit().map_err(db_error)?;
        Ok(())
    }

    pub fn inc_reprint(&self, sale_id: &str) -> KioskResult<()> {
        let conn = self.conn.lock();
        let updated = conn
            .execute(
                "UPDATE sales
                 SET reprinted_count = reprinted_count + 1
                 WHERE id = ?1",
                params![sale_id],
            )
            .map_err(db_error)?;
        if updated == 0 {
            return Err(KioskError::Db(format!("prodaja nije pronađena: {sale_id}")));
        }
        Ok(())
    }

    pub fn get_sale(&self, sale_id: &str) -> KioskResult<Option<SaleRecord>> {
        let conn = self.conn.lock();
        let stored = conn
            .query_row(
                "SELECT id, created_at, total_rsd, inserted_rsd, num_tickets,
                        reprinted_count, status, printed
                 FROM sales
                 WHERE id = ?1",
                params![sale_id],
                stored_sale_from_row,
            )
            .optional()
            .map_err(db_error)?;

        stored.map(|sale| assemble_sale(&conn, sale)).transpose()
    }

    pub fn list_sales(&self, from_ts: i64, to_ts: i64) -> KioskResult<Vec<SaleRecord>> {
        let conn = self.conn.lock();
        let stored_sales = {
            let mut stmt = conn
                .prepare(
                    "SELECT id, created_at, total_rsd, inserted_rsd, num_tickets,
                            reprinted_count, status, printed
                     FROM sales
                     WHERE created_at BETWEEN ?1 AND ?2
                     ORDER BY created_at, rowid",
                )
                .map_err(db_error)?;
            let rows = stmt
                .query_map(params![from_ts, to_ts], stored_sale_from_row)
                .map_err(db_error)?;
            let mut sales = Vec::new();
            for row in rows {
                sales.push(row.map_err(db_error)?);
            }
            sales
        };

        let mut sales = Vec::with_capacity(stored_sales.len());
        for stored in stored_sales {
            sales.push(assemble_sale(&conn, stored)?);
        }
        Ok(sales)
    }

    /// Redeem a ticket at the entrance gate. Atomic single-redemption. Not wired to a
    /// kiosk command — the separate gate scanner app owns redemption; exposed here so both
    /// share the schema. Kept intentionally.
    #[allow(dead_code)]
    pub fn redeem_ticket(&self, ticket_id: &str) -> KioskResult<bool> {
        let now = Utc::now().timestamp();
        let conn = self.conn.lock();
        let updated = conn
            .execute(
                "UPDATE tickets
                 SET redeemed_at = ?1
                 WHERE id = ?2 AND redeemed_at IS NULL",
                params![now, ticket_id],
            )
            .map_err(db_error)?;
        Ok(updated != 0)
    }

    pub fn export_csv(&self, from_ts: i64, to_ts: i64) -> KioskResult<String> {
        let conn = self.conn.lock();
        let mut stmt = conn
            .prepare(
                "SELECT s.id, t.id, t.type_code, t.label, t.price_rsd, t.issued_at,
                        t.qr_token, t.printed_at, t.redeemed_at
                 FROM tickets t
                 JOIN sales s ON s.id = t.sale_id
                 WHERE s.created_at BETWEEN ?1 AND ?2
                 ORDER BY s.created_at, t.issued_at, t.rowid",
            )
            .map_err(db_error)?;
        let rows = stmt
            .query_map(params![from_ts, to_ts], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                    row.get::<_, Option<i64>>(8)?,
                ))
            })
            .map_err(db_error)?;

        let mut csv = String::from(
            "sale_id,ticket_id,type_code,label,price_rsd,issued_at,qr_token,printed_at,redeemed_at\n",
        );
        for row in rows {
            let (
                sale_id,
                ticket_id,
                type_code,
                label,
                price_rsd,
                issued_at,
                qr_token,
                printed_at,
                redeemed_at,
            ) = row.map_err(db_error)?;
            let fields = [
                sale_id,
                ticket_id,
                type_code,
                label,
                price_rsd.to_string(),
                issued_at.to_string(),
                qr_token,
                printed_at
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
                redeemed_at
                    .map(|value| value.to_string())
                    .unwrap_or_default(),
            ];
            csv.push_str(
                &fields
                    .iter()
                    .map(|field| csv_escape(field))
                    .collect::<Vec<_>>()
                    .join(","),
            );
            csv.push('\n');
        }
        Ok(csv)
    }

    pub fn z_report(&self, from_ts: i64, to_ts: i64) -> KioskResult<ZReport> {
        let conn = self.conn.lock();
        let (count_sales_raw, total_rsd): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(total_rsd), 0)
                 FROM sales
                 WHERE created_at BETWEEN ?1 AND ?2 AND status = 'paid'",
                params![from_ts, to_ts],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(db_error)?;
        let abandoned_rsd: i64 = conn
            .query_row(
                "SELECT COALESCE(SUM(inserted_rsd), 0)
                 FROM sales
                 WHERE created_at BETWEEN ?1 AND ?2 AND status = 'abandoned'",
                params![from_ts, to_ts],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        let count_tickets_raw: i64 = conn
            .query_row(
                "SELECT COUNT(*)
                 FROM tickets t
                 JOIN sales s ON s.id = t.sale_id
                 WHERE s.created_at BETWEEN ?1 AND ?2",
                params![from_ts, to_ts],
                |row| row.get(0),
            )
            .map_err(db_error)?;

        let mut stmt = conn
            .prepare(
                "SELECT t.type_code, COUNT(*), COALESCE(SUM(t.price_rsd), 0)
                 FROM tickets t
                 JOIN sales s ON s.id = t.sale_id
                 WHERE s.created_at BETWEEN ?1 AND ?2
                 GROUP BY t.type_code
                 ORDER BY t.type_code",
            )
            .map_err(db_error)?;
        let rows = stmt
            .query_map(params![from_ts, to_ts], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(db_error)?;
        let mut by_type = Vec::new();
        for row in rows {
            let (type_code, count, sum) = row.map_err(db_error)?;
            by_type.push((type_code, checked_u32(count, "ticket type count")?, sum));
        }

        Ok(ZReport {
            from_ts,
            to_ts,
            count_sales: checked_u32(count_sales_raw, "sale count")?,
            count_tickets: checked_u32(count_tickets_raw, "ticket count")?,
            total_rsd,
            abandoned_rsd,
            by_type,
        })
    }
}

fn stored_sale_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredSale> {
    Ok(StoredSale {
        id: row.get(0)?,
        created_at: row.get(1)?,
        total_rsd: row.get(2)?,
        inserted_rsd: row.get(3)?,
        num_tickets: row.get(4)?,
        reprinted_count: row.get(5)?,
        status: row.get(6)?,
        printed: row.get::<_, i64>(7)? != 0,
    })
}

fn assemble_sale(conn: &Connection, stored: StoredSale) -> KioskResult<SaleRecord> {
    let tickets = load_tickets(conn, &stored.id)?;
    Ok(SaleRecord {
        id: stored.id,
        created_at: stored.created_at,
        total_rsd: stored.total_rsd,
        inserted_rsd: stored.inserted_rsd,
        num_tickets: checked_u32(stored.num_tickets, "num_tickets")?,
        tickets,
        reprinted_count: checked_u32(stored.reprinted_count, "reprinted_count")?,
        status: stored.status,
        printed: stored.printed,
    })
}

fn load_tickets(conn: &Connection, sale_id: &str) -> KioskResult<Vec<PrintedTicket>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, type_code, label, price_rsd, issued_at, qr_token
             FROM tickets
             WHERE sale_id = ?1
             ORDER BY issued_at, rowid",
        )
        .map_err(db_error)?;
    let rows = stmt
        .query_map(params![sale_id], |row| {
            Ok(PrintedTicket {
                id: row.get(0)?,
                type_code: row.get(1)?,
                label: row.get(2)?,
                price_rsd: row.get(3)?,
                issued_at: row.get(4)?,
                qr_token: row.get(5)?,
            })
        })
        .map_err(db_error)?;
    let mut tickets = Vec::new();
    for row in rows {
        tickets.push(row.map_err(db_error)?);
    }
    Ok(tickets)
}

fn csv_escape(field: &str) -> String {
    if field.contains(|ch| matches!(ch, ',' | '"' | '\n' | '\r')) {
        format!("\"{}\"", field.replace('"', "\"\""))
    } else {
        field.to_owned()
    }
}
