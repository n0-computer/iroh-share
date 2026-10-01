# Publishing UX plan

1. Make Data the primary publishing screen. Local publishing uses file/folder
   selection or drag-and-drop; remote publishing uses Import ticket.
   Offer an optional name and show import progress, public URLs and Copy ticket.
2. Manage content-linked names with Data, including fixed content URL targets and
   unavailable linked data. Show the public name URL and publication state. Keep
   ordinary URL names and downloads in separate collapsed sections.
3. Add Refresh for named local directories and Update from ticket for existing
   content. Preserve the data ID, name keys and public name URLs across updates.
   Validate eligibility on the daemon and preserve editable input on failures.
4. Export ticket imports into a publicly visible working directory, separate from
   private daemon state and blob storage. Default to Downloads/Iroh Share with a
   daemon --import-dir override. Each imported version receives a fresh folder;
   previous exported files are retained.
5. Accept dropped .ticket/.sendme text files for new imports or an open update
   dialog. Always review before fetching. Preserve offline gating and daemon-side
   path completion. Require the same-filesystem setting for local folder drops.
6. Verify refresh through RPC, new and replacement ticket imports, persistence,
   retained names and files, name classification, clipboard behavior, dropped
   tickets, and reconnection. Document capabilities in UI.md; frontends can differ.
