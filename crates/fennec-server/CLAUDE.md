# CLAUDE.md

fennec-server's invariants -- every way it stops is the one shutdown, and a start as root becomes its user before it touches a file -- for code in `crates/fennec-server/`. The rules every change meets, and where every other area's are, are in the root's [CLAUDE.md](../../CLAUDE.md).

## Invariants

### Durability

**[Every way a server stops is the one shutdown.](../../docs/design/durability.md#the-one-shutdown)**
A signal's handler -- on Windows every console event's but a service's
logoff's (`SetConsoleCtrlHandler`, Ctrl+C turned on), the close's, a
user's logoff's and the system shutdown's syncing before each wait and
holding until the exit --, `--exit-on-stdin-close`'s end of stdin
(`request_shutdown`) and a failure of what runs beside the database
(`stop_with`, its exit code) set one flag, `SHUTDOWN`; the syncer, a file's
or a node's, looks every `LOOK` whatever `--sync` is, syncs, checkpoints
(only once `serving`, and exiting 0, `checkpoints`) and exits holding the
write lock. A new way down sets that flag and never exits on its own. What
fails before anything writes the file or runs beside it -- a flag, the open,
a port, both listeners bound straight after the open -- exits where it
fails, having written only `<file>.lock` and what the open does.

### Integrations and deploy

**[Started as root, a server becomes its user before it touches a
file.](../../docs/design/integrations.md#the-images-user)** Told one by
`--user` or `FENNEC_USER` (the image's, `65534:65534`), `main` calls
`user::start_as` straight after the arguments: before any file is opened --
the audit log, a secret, the policy, the CAs and `<file>.lock` are each
opened after it, never in the arguments' loop -- and before any thread;
`--ping` and `--mint-token` call `become_user` alone. `become_user` drops
the supplementary groups, the group, then the user, and fails the start
(2) unless the ids are the user's alone and `setuid(0)` fails. The
hand-over before it changes owners alone, through a descriptor of the inode
`user::plan` looked at (`hand_over`), and only of the directory the server
writes in and the database's files in it (`Writes::is_ours`, regular, one
name each); `plan` refuses `/`, a path through a link, a directory on `/`'s
file system or shared, and one holding anything else but `lost+found` --
said, never fatal, and the only thing a refused flag may follow. Started as
another user, the variable is passed over unread and a `--user` it is not is
refused. A file the start opens goes after the drop; a new kind of file in
a data directory is named by `Writes::is_ours`.
