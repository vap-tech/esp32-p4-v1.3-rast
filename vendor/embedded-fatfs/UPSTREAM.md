Source: https://github.com/MabezDev/embedded-fatfs
Revision: 7961cd4eb64de51818d8cdda991713784fa98366
License: MIT (LICENSE retained).
Only the embedded-fatfs crate is included; host examples/resources are omitted.

Local changes:
- src/dir.rs: ShortNameGenerator::new used name[1..], which panicked whenever
  the first filename character occupied multiple UTF-8 bytes. Find the last
  ASCII dot in the complete string and exclude position zero instead.
  Covered by storage::volume's Cyrillic/emoji creation, rename/remount test.
- LFN validation counts UTF-16 code units rather than UTF-8 bytes and accepts
  supplementary Unicode characters. Encoding already used encode_utf16;
  the older validator needlessly rejected surrogate-pair filenames.
- Remove deprecated core::u8/u16/u32 imports for current stable Rust.
- Dir::rename resolves destination relative to dst_dir and passes the resolved
  directory into rename_internal. Upstream computed e_dst but discarded it,
  moving a nested rename into the original destination directory instead.
- FileContext/DirEntry expose directory-entry position for alias-safe open-file
  identity and ancestor locks. Contexts remain private to our file table.
- File::suspend/resume_exclusive preserve cursor and dirty entry without IO;
  the wrapper exclusively owns the volume, enforces writer/reader exclusions,
  protects open entries/ancestors from rename/remove, and disables atime.
  Close/sync explicitly flush; aborted create flushes first cluster before unlink.
- FileSystem::flush_preserving_dirty saves FSInfo while unfinished writers remain;
  ordinary flush still clears the dirty flag as before.
