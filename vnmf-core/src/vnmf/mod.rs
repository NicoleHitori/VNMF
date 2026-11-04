// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (C) 2026 void_nicole (Nicole Hitori) + AI-assist

mod aux_record;
mod entry_record;
mod page_record;
mod preview_record;
mod record_crc;

pub use aux_record::{AuxFlags, AuxKind, AuxRecord};
pub use entry_record::EntryRecord;
pub use page_record::{PageBitFlags, PageRecord, PageRole};
pub use preview_record::PreviewRecord;
pub use record_crc::RecordCrc;
