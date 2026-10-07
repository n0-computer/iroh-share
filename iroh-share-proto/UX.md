# Iroh Share UX foundations

This is the shared product contract for the GUI, TUI, and other control clients.
Use [UI.md](UI.md) for RPC and live-state details. Layouts and input devices may
vary; the workflows and meanings below must agree.

## Purpose and priorities

The main purpose is publishing your own content. Data is the primary screen.
Sharing other people's content is supported but is not the primary workflow.
Standalone URL names and ordinary downloads are secondary, collapsed by default.
Settings must not compete with publishing for primary screen space.

## Publish locally or import remotely

- Local: select or drop a file/directory on the daemon's filesystem. Show import
  progress, then the public content URL and a Copy ticket action.
- Remote: paste a sendme collection ticket and import it to the daemon. No remote
  destination path is required. Explain that the sender must remain online until
  fetching completes. The GUI also accepts .ticket/.sendme file drops and reviews
  them before starting. The TUI uses paste; terminal-dependent native file drag
  behavior is not a required interaction.
- Offer a name as part of publishing or immediately afterward. It is optional;
  failure to create a name must not conceal successfully added content or cause
  the UI to import it again. Provide a clear way to retry just the name operation.

The public working directory is separate from the private blob/state directory.
Imports default to Downloads/Iroh Share on the daemon, with a home-directory
fallback, and daemon --import-dir overrides it. Display the daemon-reported path.
Each imported version gets a fresh destination. Existing exported files remain.
Native pickers, local file drops and Open folder require shared filesystem access;
tickets work independently of that setting. Path completion always runs remotely.

## Data and its names form one publishing unit

All names targeting content belong in Data, including linked jobs, fixed blake3.net
URLs with subpaths, and names whose linked data is unavailable. Use
NameTarget::is_content() to classify them consistently. Ordinary URL redirects
belong in the collapsed Names section.

Show a friendly label, public pkarr URL, publication state, and whether the name
follows data or points at a fixed URL. The label is local metadata, not a chosen
DNS hostname. Never require users to type internal data IDs; select by path.
Creating, retargeting, copying/opening and removing content names must be possible
from Data. A named item's stable public URL is its pkarr URL; also expose its
current immutable blake3.net URL and full sendme-compatible ticket.

## Update without replacing identity

- Refresh rescans a named local shared directory. It retains the data ID and all
  name identities. Automatic change monitoring remains active as well.
- Update from ticket changes existing content using a new sendme ticket. It
  retains the same data ID and linked name keys. Names point at the new content
  once it is ready. Existing exported files and the preceding seeded collection
  are retained. A failed update stays visible and can be retried.
- Only idle or failed data can accept another ticket update. The daemon validates
  refresh eligibility: a directory share, a linked name, and no active import.

## Feedback, safety, and recovery

An accepted RPC starts work; it does not mean data or a name has finished publishing.
Use Watch states for progress and publication status. Preserve input on errors,
disable duplicate submissions, and do not silently replay operations on reconnect.
Discard selections and pending confirmations tied to a disconnected daemon.

Removing tracked data leaves files and name identities in place. Explain that
linked names stop following it. Removing a name deletes its signing identity;
confirm separately and explain that cached published records can remain.
Do not show job IDs by default. Always copy full URLs/tickets, even when labels
are abbreviated. Keep mutation errors visible rather than hiding them in logs.

## Interface controls

The GUI exposes Import ticket and per-row Update from ticket, Add name,
Copy ticket, and eligible Refresh directory actions. Standalone names and
ordinary downloads use disclosure sections.

The browser frontend follows the GUI layout and wording. It has no local
filesystem access: no folder pickers, folder drops, Open directory or Refresh.
Ticket file drops and pasted tickets work as in the GUI.

The TUI uses s to publish a path, i to import a ticket, u to update selected data,
r to refresh, n to name data, and t to copy a ticket. Tab opens Data's content-name
view; e retargets a name, with path selection for data targets. N expands/collapses
standalone names; D expands/collapses downloads and d starts a download there.
c/o copy/open the selected public URL. Names on data rows and the content-name
view must agree with the GUI's classification and publication state.

GUI row actions stay in one horizontal row. Use compact copy, open, and trash
icons with descriptive tooltips and accessible labels. Removal still requires
confirmation; do not hide common actions in a More menu.

GUI table headers and cells align to the top left; row actions align to the top
left beneath the Actions header. Public URLs have explicit copy and open buttons. In tables, show names
as their public URL with a small inline publication status, without a separate
alias line or column.

Within each GUI table line, text shares a baseline and icons share a vertical
center. Keep the first line at the top of multiline rows. Local paths expose
an Open directory icon that uses the OS file manager. Both publishing paths
and download destinations support Tab completion against the daemon's filesystem;
download destinations offer directories only.

GUI directory-open controls appear to the right of the path when the daemon-local
setting is enabled, and are enabled when the path exists on the GUI's computer.
Abbreviated URLs use monospace text and a consistent width so link icons align.

GUI creation starts from an Add content or Add name row in the corresponding
table, after all existing entries. Creation fields live directly in these rows;
do not open dialogs. The content row selects local publishing, ticket import,
or download and supports path selection and Tab completion. Bound names are
added inline in the content table; standalone names have label and URL fields
in their final table row.

Names bound to existing data appear only in that content row, with an inline
remove-name action and confirmation. Do not duplicate them in a separate names
area. Fixed content URLs and names whose data is unavailable remain separately
manageable. Removing a name leaves the shared data intact.

Path completion is triggered with Tab, without a separate Complete button.
Show candidates in an anchored dropdown over the table; selecting a candidate
returns focus to the path field. Escape or clicking outside dismisses it.

GUI content creation does not ask for a name. Create the content first, then
use its normal Add name action to associate a name.

Content and Names are collapsible GUI panels. Content starts expanded; Names
starts collapsed. Content-bound names remain with their data inside Content.

Add name on a content row immediately creates and attaches a fresh name, without
asking for an alias. Generate the internal label from the path basename (or URL
host for standalone names), adding a suffix to avoid existing labels. Standalone
name creation only asks for its target URL.

Directory shares put their contents at the collection root by default: sharing
foo/bar produces index.html, not bar/index.html. Include directory name is an
opt-in checkbox in the GUI, Ctrl+R in the TUI share prompt, and
--include-directory-name for CLI share. Single-file shares retain their filename.
The layout choice persists with the share and applies to refreshes. Saved shares
without a layout option include the directory name to preserve their layout.

The Names panel offers Export all pkarr names, including content-bound names.
Use a local save-file picker and identify that the ZIP includes private keys.
No aliases are needed in the backup; each entry is identified by its public key.

Standalone name creation is an advanced DNS feature: show a multiline monospace
editor prefilled with `@ 300 IN HTTPS 0 example.com.`.
Use one record per line in owner/TTL/IN/type/value form; @ is the pkarr key root.
Content-bound names continue to generate their records automatically.

The advanced DNS editor uses a wide viewport and horizontal scrolling without
wrapping, keeping each DNS record on a single visual line.

Edit DNS records in the existing name's row, with Save and Cancel beside it.
The final new-name row keeps a separate draft and is never reused for editing.

Iroh Link Gateway resolves apex HTTPS records, not URI-only records. The
default DNS template includes only an apex HTTPS record for an
HTTPS origin. Full-URL URI redirects with paths or queries require resolver
support and must not be represented as hostname-only aliases.
