// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

//! Per-record checksum of a table.
//!
//! The table CRC in the header only says "table intact / not". A CRC on every record lets us
//! determine *which* records survived — so a damaged table can be rebuilt record by record
//! from the primary and tail copies (see `recovery::salvage_metadata`).

use zerocopy::{Immutable, IntoBytes};

/// A record with its own CRC32 field. The CRC is computed over all bytes of the record with the CRC field zeroed.
pub trait RecordCrc: IntoBytes + Immutable + Copy {
    fn stored_crc(&self) -> u32;
    fn set_stored_crc(&mut self, crc: u32);

    fn calculate_crc(&self) -> u32 {
        let mut copy = *self;
        copy.set_stored_crc(0);
        crc32fast::hash(copy.as_bytes())
    }

    /// Writes the current CRC into the record field.
    fn seal(&mut self) {
        let crc = self.calculate_crc();
        self.set_stored_crc(crc);
    }

    /// The record is not damaged.
    fn is_intact(&self) -> bool {
        self.stored_crc() == self.calculate_crc()
    }
}
