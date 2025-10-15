use zerocopy::{byteorder::big_endian, byteorder::native_endian, *};

pub const HEADER_STRING: &'static [u8; 16] = b"SQLite format 3\0";
pub const RESERVED_ZEROES: &'static [u8; 20] = b"\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Debug)]
#[repr(C)]
pub struct SqliteHeader {
    pub header_string: [u8; 16],
    pub page_size: big_endian::U16,
    pub file_format_write_version: u8,
    pub file_format_read_version: u8,
    pub unused_reversed_space_at_end_of_page: u8,
    pub maximum_embedded_payload_fraction: u8,
    pub minimum_embedded_payload_fraction: u8,
    pub leaf_payload_fraction: u8,
    pub file_change_counter: big_endian::U32,
    pub database_file_size_in_pages: big_endian::U32,
    pub first_free_trunk_page_number: big_endian::U32,
    pub total_number_of_freelist_pages: big_endian::U32,
    pub schema_cookie: big_endian::U32,
    pub schema_format_number: big_endian::U32,
    pub default_page_cache_size: big_endian::U32,
    pub large_root_btree_page_number: big_endian::U32,
    pub database_text_encoding: big_endian::U32,
    pub user_version: big_endian::U32,
    pub incremental_vacuum_mode: big_endian::U32,
    pub application_id: big_endian::U32,
    pub reserved: [u8; 20],
    pub version_valid_for: big_endian::U32,
    pub sqlite_version_number: big_endian::U32,
}

impl SqliteHeader {
    pub fn empty_wal_header() -> Self {
        SqliteHeader {
            header_string: HEADER_STRING.clone(),
            page_size: 4096.into(),
            file_format_write_version: 2, // wal
            file_format_read_version: 2,  // wal
            unused_reversed_space_at_end_of_page: 0,
            maximum_embedded_payload_fraction: 64,
            minimum_embedded_payload_fraction: 32,
            leaf_payload_fraction: 32,
            file_change_counter: 1.into(),
            database_file_size_in_pages: 1.into(),
            first_free_trunk_page_number: 0.into(),
            total_number_of_freelist_pages: 0.into(),
            schema_cookie: 0.into(),
            schema_format_number: 0.into(),
            default_page_cache_size: 0.into(),
            large_root_btree_page_number: 0.into(),
            database_text_encoding: 0.into(), //  1.into(), // utf 8
            user_version: 0.into(),
            incremental_vacuum_mode: 0.into(),
            application_id: 0.into(),
            reserved: RESERVED_ZEROES.clone(),
            version_valid_for: 1.into(),
            sqlite_version_number: SQLITE_VERSION_NUMBER.into(),
        }
    }
}

// todo: assert this??
pub const SQLITE_HEADER_SIZE: usize = 100;
pub const SQLITE_VERSION_NUMBER: u32 = 3050002;

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Debug)]
#[repr(C)]
pub struct BtreePageHeader {
    pub page_type: u8,
    pub first_freeblock: big_endian::U16,
    pub number_of_cells: big_endian::U16,
    pub start_cell_content_area: big_endian::U16,
    pub fragmented_free_bytes_in_content_area: u8,
}

pub const BTREE_PAGE_HEADER_SIZE: usize = 8;

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Debug)]
#[repr(C)]
pub struct InteriorBtreePageHeader {
    pub page_type: u8,
    pub first_freeblock: big_endian::U16,
    pub number_of_cells: big_endian::U16,
    pub start_cell_content_area: big_endian::U16,
    pub fragmented_free_bytes_in_content_area: u8,
    pub rightmost_pointer: big_endian::U32,
}

pub const INTERIOR_BTREE_PAGE_HEADER_SIZE: usize = 12;

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Debug)]
#[repr(C)]
pub struct WalHeader {
    pub magic_number: big_endian::U32,
    pub file_format: big_endian::U32,
    pub page_size: big_endian::U32,
    pub checkpoint_sequence_number: big_endian::U32,
    pub salt1: big_endian::U32,
    pub salt2: big_endian::U32,
    pub checksum1: big_endian::U32,
    pub checksum2: big_endian::U32,
}

pub const WAL_HEADER_SIZE: usize = 32;

pub const WAL_HEADER_MAGIC_NUMBER_LITTLE_ENDIAN: u32 = 0x377f0682;
pub const WAL_FILE_FORMAT_VERSION: u32 = 3007000;

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Debug)]
#[repr(C)]
pub struct WalFrameHeader {
    pub page_number: big_endian::U32,
    pub database_size_after_commit: big_endian::U32,
    pub salt1: big_endian::U32,
    pub salt2: big_endian::U32,
    pub checksum1: big_endian::U32,
    pub checksum2: big_endian::U32,
}

pub const WAL_FRAME_HEADER_SIZE: usize = 24;

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Debug, Clone)]
#[repr(C)]
pub struct WalIndexInfo {
    pub version_number: native_endian::U32, // The WAL-index format version number. Always 3007000.

    pub padding: [u8; 4], // Unused padding space. Must be zero.
    pub change_counter: native_endian::U32, // Unsigned integer counter, incremented with each transaction.
    pub is_init: u8, // The "isInit" flag. 1 when the shm file has been initialized.

    pub big_endian_checksums: u8, // True if the WAL file uses big-endian checksums. 0 if the WAL uses little-endian checksums.

    pub page_size: native_endian::U16, // The database page size in bytes, or 1 if the page size is 65536.
    pub mx_frame: native_endian::U32,  // Number of valid and committed frames in the WAL file.
    pub n_page: native_endian::U32,    // Size of the database file in pages.

    pub last_frame_checksum1: big_endian::U32,
    pub last_frame_checksum2: big_endian::U32,

    pub salt1: big_endian::U32, // The two salt values copied from the WAL file header. These values are in the byte-order of the WAL file, which might be different from the native byte-order of the machine.
    pub salt2: big_endian::U32,

    pub checksum1: native_endian::U32,
    pub checksum2: native_endian::U32,
    // pub cksum: [u8; 8], // A checksum over bytes 0 through 39 of this header.
}

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Debug)]
#[repr(C)]
pub struct WalIndexHeader {
    pub info: WalIndexInfo,
    pub info_copy: WalIndexInfo,
    pub tail: WalIndexHeaderTail,
}

#[derive(FromBytes, IntoBytes, KnownLayout, Immutable, Unaligned, Debug, Clone)]
#[repr(C)]
pub struct WalIndexHeaderTail {
    pub n_backfill: native_endian::U32,
    pub readmarks: [native_endian::U32; 5],
    pub unused_for_locks: [u8; 8],
    pub n_backfill_attempted: native_endian::U32,
    pub unused_end: [u8; 4],
}

pub const WAL_INDEX_FORMAT_VERSION: u32 = 3007000;

pub const WAL_INDEX_INFO_SIZE: usize = 48;

pub const WAL_INDEX_HEADER_SIZE: usize = 136;

pub const WAL_INDEX_INITIAL_SIZE: usize = 32768;
