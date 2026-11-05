# SMB frame fixtures

The `.hex` files in this directory are deterministic parser-conformance frames.
They intentionally use recognizable payload bytes so parser failures are easy
to diagnose; they are not presented as packet captures.

Wire interoperability is validated separately by the native Samba CI matrix,
which exercises signed SMB 2.1, encrypted SMB 3.0/AES-CCM, and encrypted SMB
3.1.1/GCM connections, including authentication, read/write, COPYCHUNK,
rename/delete, and paged directory enumeration.
