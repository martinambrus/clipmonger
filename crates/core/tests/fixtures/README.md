`backup-zip4.zip` was written by the unchanged `Store::backup_zip` at
94b710e with zip 4.6.1, before the dependency update. Keep this archive
as a compatibility fixture; regenerating it with the current writer would
lose the check that older backups still restore.

The fixed seed is in `../support/backup_fixture.rs`: two text clips (one
pinned), a 262,208-byte blob, and a permanent with a fixed ID and revision.
The temporary generator used the public store API, then fixed the permanent
ID and revision with SQL before calling the unchanged backup writer.
`backup_compat.rs` checks the restored models and payload bytes against
that shared seed. The archive is 3,294 bytes.

SHA-256: `86f551acae19c198a027d2d6bf4b0b4e0729cef889e5f1888553d541f35513a5`.
