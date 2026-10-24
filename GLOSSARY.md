# RustCommander

A dual-pane file manager for Windows. Each panel shows one folder; commands act on the marked
entries or the one under the cursor.

## Language

### Where a panel is

**Location**:
The address of a folder a panel can show: a local path (archives and network servers included)
or a folder on an SFTP server.
_Avoid_: path (when an SFTP folder is meant), URL

**Folder kind**:
What sort of folder a location is, and so what can be done there: local folder, archive,
server list or SFTP folder.
_Avoid_: mode, panel type

**Local folder**:
A folder in the Windows file system, including folders on network shares.

**Archive**:
A zip, 7z, rar, tar, cab or iso file shown as a folder. Read-only.
_Avoid_: packed file, container

**Server list**:
The list of shares of a network server (`\\server`). Only for navigating.
_Avoid_: share view, network folder

**SFTP folder**:
A folder on an SFTP server, reached through ssh.
_Avoid_: remote folder (in the UI), FTP

### Asking before acting

**Action**:
A user command that the folder kind may allow or refuse, such as copy, delete or pack.
_Avoid_: op, operation (an operation is the work a job does)

**Refusal**:
The answer that an action isn't possible in a folder kind, with the notice the user sees.
_Avoid_: error (nothing failed)
