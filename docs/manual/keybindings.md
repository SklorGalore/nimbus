# Keyboard shortcuts and commands

Every command in Nimbus, with its **id** (what you write in the `[keys]`
table of `config.toml`), its **title** (what the command palette and
menus show) and its default keys. This list is made from the editor's
own command table, so it matches the build it describes (version 0.10.0).

- Commands without a key are run from the command palette
  (`Cmd+Shift+P` / `Ctrl+Shift+P`).
- Commands marked † are not listed in the palette: plain cursor
  movement and the typing keys, which nobody looks up by name.
- *(find bar)* marks keys that apply only while the find bar is open.
  `Cmd+Enter` / `Ctrl+Enter` is Replace All in the find bar and Insert
  Line Below everywhere else.
- `Cmd+K Cmd+0` means: press `Cmd+K`, let go, then press `Cmd+0` within
  two seconds.
- In the palette, macOS keys are drawn with symbols: `⌘` Command, `⌥`
  Option, `⌃` Control, `⇧` Shift.
- To change a key, see [Settings: key bindings](settings.md#key-bindings).

Some defaults may be taken by your system before Nimbus sees them. On
Linux, some window managers use `Alt+Up` and `Alt+Down` (Move Line) and
`Ctrl+Alt+Left` and `Ctrl+Alt+Right` (Focus Group); some Windows graphics
drivers use `Ctrl+Alt+Up` and `Ctrl+Alt+Down` (Add Cursor) to rotate the
screen. Rebind them, or use the palette.

## Mouse

| Action | macOS | Windows and Linux |
|---|---|---|
| Place the cursor | click | click |
| Extend the selection | `Shift`+click | `Shift`+click |
| Select a word, a line | double-click, triple-click | double-click, triple-click |
| Add or remove a cursor | `Option`+click | `Alt`+click |
| Column selection | `Shift+Option`+drag | `Shift+Alt`+drag |
| Fold or unfold a line | click the marker in the gutter | click the marker in the gutter |
| Unfold | click the `… N lines` chip | click the `… N lines` chip |
| Change this file's indentation | click `Spaces: 4` or `Tabs` in the status bar | click `Spaces: 4` or `Tabs` in the status bar |
| Change line endings | click `LF` or `CRLF` in the status bar | click `LF` or `CRLF` in the status bar |
| Reopen or save in another encoding | click the encoding (`UTF-8`) in the status bar | click the encoding (`UTF-8`) in the status bar |
| Resize the sidebar or a group | drag its edge | drag its edge |
| Jump to a line pinned by sticky scroll | click it | click it |
| Scroll with the minimap | click or drag in it | click or drag in it |
| Follow a link in the Markdown preview | click it | click it |
| Turn Follow Tail on or off | click `Follow Tail` or `Following` in the status bar | click `Follow Tail` or `Following` in the status bar |
| New File, New Folder, Rename, Move to Trash, Reveal | right-click a row of the file tree | right-click a row of the file tree (Windows); the palette on Linux |

## In the file tree

These keys work while the file tree has the keyboard (`Cmd+Shift+E` /
`Ctrl+Shift+E`, or click in it). They follow Finder on macOS.

| Action | macOS | Windows and Linux |
|---|---|---|
| Move | `Up`, `Down`, `PageUp`, `PageDown` | `Up`, `Down`, `PageUp`, `PageDown` |
| First or last row | `Home`, `End` | `Home`, `End` |
| Open a folder, or step into it | `Right` | `Right` |
| Close a folder, or go to its parent | `Left` | `Left` |
| Open a file, or open or close a folder | `Cmd+Down` or `Space` | `Enter` |
| Rename | `Enter` or `F2` | `F2` |
| Move to Trash | `Cmd+Backspace` or `Delete` (forward delete) | `Delete` |
| Give the keyboard back to the text | `Escape` | `Escape` |

## Commands by area

### Files, tabs and the app

| Title | Id | macOS | Windows and Linux |
|---|---|---|---|
| Go to File… | `file.quickOpen` | `Cmd+P` | `Ctrl+P` |
| Command Palette | `app.palette` | `Cmd+Shift+P` | `Ctrl+Shift+P` |
| New File | `file.new` | `Cmd+N`, `Cmd+T` | `Ctrl+N`, `Ctrl+T` |
| Open File… | `file.open` | `Cmd+O` | `Ctrl+O` |
| Open Recent… | `file.openRecent` | `Cmd+Shift+O` | `Ctrl+Shift+O` |
| Reopen Closed Tab | `file.reopenClosed` | `Cmd+Shift+T` | `Ctrl+Shift+T` |
| Clear Recent Files | `file.clearRecent` | — | — |
| New File in Tree… | `tree.newFile` | — | — |
| New Folder in Tree… | `tree.newFolder` | — | — |
| Rename in Tree… | `tree.rename` | `F2` | `F2` |
| Move to Trash | `tree.delete` | — | — |
| Reveal in Finder (macOS), Reveal in Explorer (Windows), Reveal in File Manager (Linux) | `tree.reveal` | — | — |
| Undo Move | `tree.undoMove` | — | — |
| Search in Folder | `tree.searchInFolder` | — | — |
| Add Folder to Workspace… | `workspace.addFolder` | — | — |
| Remove Folder from Workspace | `workspace.removeFolder` | — | — |
| Save Workspace As… | `workspace.saveAs` | — | — |
| Open Workspace… | `workspace.open` | — | — |
| Save Workspace Here | `workspace.saveHere` | — | — |
| Open Project Workspace | `workspace.openHere` | — | — |
| Reopen Last Session | `session.reopenLast` | — | — |
| Save | `file.save` | `Cmd+S` | `Ctrl+S` |
| Save As… | `file.saveAs` | `Cmd+Shift+S` | `Ctrl+Shift+S` |
| Reopen with Encoding… | `file.reopenWithEncoding` | — | — |
| Save with Encoding… | `file.saveWithEncoding` | — | — |
| Close Tab | `file.close` | `Cmd+W` | `Ctrl+W` |
| Quit | `app.quit` | `Cmd+Q` | `Ctrl+Q` |
| Next Tab | `tab.next` | `Ctrl+Tab` | `Ctrl+Tab` |
| Previous Tab | `tab.previous` | `Ctrl+Shift+Tab` | `Ctrl+Shift+Tab` |
| Go to Tab 1 | `tab.goto1` | `Cmd+1` | `Ctrl+1` |
| Go to Tab 2 | `tab.goto2` | `Cmd+2` | `Ctrl+2` |
| Go to Tab 3 | `tab.goto3` | `Cmd+3` | `Ctrl+3` |
| Go to Tab 4 | `tab.goto4` | `Cmd+4` | `Ctrl+4` |
| Go to Tab 5 | `tab.goto5` | `Cmd+5` | `Ctrl+5` |
| Go to Tab 6 | `tab.goto6` | `Cmd+6` | `Ctrl+6` |
| Go to Tab 7 | `tab.goto7` | `Cmd+7` | `Ctrl+7` |
| Go to Tab 8 | `tab.goto8` | `Cmd+8` | `Ctrl+8` |
| Go to Last Tab | `tab.gotoLast` | `Cmd+9` | `Ctrl+9` |
| Reload Config | `app.reloadConfig` | — | — |
| Open Config File | `app.openConfig` | — | — |
| Show Default Config | `app.showDefaultConfig` | — | — |
| Check for Updates | `app.checkUpdates` | — | — |
| Open Latest Release Page | `app.openRelease` | — | — |
| Install nimbus Command in PATH | `app.installCommand` | — | — |
| Uninstall nimbus Command | `app.uninstallCommand` | — | — |
| Install Update | `app.installUpdate` | — | — |
| Restart to Update | `app.restartToUpdate` | — | — |
| Cancel Update | `app.cancelUpdate` | — | — |
| Roll Back Update | `app.rollBackUpdate` | — | — |

### Compare and merge

| Title | Id | macOS | Windows and Linux |
|---|---|---|---|
| Compare with Saved | `diff.compareSaved` | — | — |
| Compare with File… | `diff.compareFile` | — | — |
| Compare with Tab… | `diff.compareTab` | — | — |
| Compare with HEAD | `diff.compareHead` | — | — |
| Compare with Revision… | `diff.compareRevision` | — | — |
| Compare Folders… | `diff.compareFolders` | — | — |
| Open Git Window | `git.openWindow` | — | — |
| Ignore Whitespace (Off, Ends, All) | `diff.ignoreWhitespace` | — | — |
| Ignore Case in Compare | `diff.ignoreCase` | — | — |
| Ignore Blank Lines in Compare | `diff.ignoreBlankLines` | — | — |
| Next Change | `diff.next` | `F7` | `F7` |
| Previous Change | `diff.previous` | `Shift+F7` | `Shift+F7` |
| Toggle Unified Compare | `diff.toggleUnified` | — | — |
| Close Compare or Merge | `diff.close` | — | — |
| Merge Files… | `merge.files` | — | — |
| Merge with Base… | `merge.withBase` | — | — |
| Take Left | `merge.takeLeft` | — | — |
| Take Right | `merge.takeRight` | — | — |
| Take Both | `merge.takeBoth` | — | — |
| Take Both, Right First | `merge.takeBothReversed` | — | — |
| Next Conflict | `merge.nextConflict` | `F8` | `F8` |
| Previous Conflict | `merge.previousConflict` | `Shift+F8` | `Shift+F8` |
| Resolve Conflicts | `merge.resolveMarkers` | — | — |
| Merge with Disk | `merge.withDisk` | — | — |

### Editing

| Title | Id | macOS | Windows and Linux |
|---|---|---|---|
| Undo | `edit.undo` | `Cmd+Z` | `Ctrl+Z` |
| Redo | `edit.redo` | `Cmd+Shift+Z` | `Ctrl+Shift+Z`, `Ctrl+Y` |
| Cut | `edit.cut` | `Cmd+X` | `Ctrl+X` |
| Copy | `edit.copy` | `Cmd+C` | `Ctrl+C` |
| Paste | `edit.paste` | `Cmd+V` | `Ctrl+V` |
| Select All | `edit.selectAll` | `Cmd+A` | `Ctrl+A` |
| Add Next Occurrence | `selection.addNextOccurrence` | `Cmd+D` | `Ctrl+D` |
| Add Cursor Above | `selection.addCursorAbove` | `Cmd+Option+Up` | `Ctrl+Alt+Up` |
| Add Cursor Below | `selection.addCursorBelow` | `Cmd+Option+Down` | `Ctrl+Alt+Down` |
| Insert Line Break † | `edit.newline` | `Enter`, `Shift+Enter` | `Enter`, `Shift+Enter` |
| Indent / Insert Tab † | `edit.tab` | `Tab` | `Tab` |
| Outdent | `edit.outdent` | `Shift+Tab` | `Shift+Tab` |
| Delete Backward † | `edit.deleteBack` | `Backspace`, `Shift+Backspace` | `Backspace`, `Shift+Backspace` |
| Delete Forward † | `edit.deleteForward` | `Delete`, `Shift+Delete` | `Delete`, `Shift+Delete` |
| Delete Word Backward | `edit.deleteWordBack` | `Option+Backspace` | `Ctrl+Backspace` |
| Delete Word Forward | `edit.deleteWordForward` | `Option+Delete` | `Ctrl+Delete` |
| Delete to Line Start | `edit.deleteToLineStart` | `Cmd+Backspace` | — |
| Cancel † | `edit.cancel` | `Escape` | `Escape` |

### Lines, comments and text

| Title | Id | macOS | Windows and Linux |
|---|---|---|---|
| Toggle Line Comment | `edit.toggleLineComment` | `Cmd+/` | `Ctrl+/` |
| Toggle Block Comment | `edit.toggleBlockComment` | `Cmd+Shift+/` | `Ctrl+Shift+/` |
| Duplicate Line or Selection | `edit.duplicate` | `Cmd+Shift+D` | `Ctrl+Shift+D` |
| Move Line Up | `edit.moveLineUp` | `Option+Up` | `Alt+Up` |
| Move Line Down | `edit.moveLineDown` | `Option+Down` | `Alt+Down` |
| Delete Line | `edit.deleteLine` | `Cmd+Shift+K` | `Ctrl+Shift+K` |
| Join Lines | `edit.joinLines` | `Cmd+J` | `Ctrl+J` |
| Insert Line Below | `edit.insertLineBelow` | `Cmd+Enter` | `Ctrl+Enter` |
| Insert Line Above | `edit.insertLineAbove` | `Cmd+Shift+Enter` | `Ctrl+Shift+Enter` |
| Select Line | `select.line` | `Cmd+L` | `Ctrl+L` |
| Expand Selection | `select.expand` | `Ctrl+Shift+Right` | `Alt+Shift+Right` |
| Shrink Selection | `select.shrink` | `Ctrl+Shift+Left` | `Alt+Shift+Left` |
| Go to Matching Bracket | `cursor.matchingBracket` | `Cmd+Shift+\` | `Ctrl+Shift+\` |
| Select to Matching Bracket | `select.toMatchingBracket` | — | — |
| Transform to Upper Case | `edit.upperCase` | — | — |
| Transform to Lower Case | `edit.lowerCase` | — | — |
| Transform to Title Case | `edit.titleCase` | — | — |
| Sort Lines | `edit.sortLines` | — | — |
| Sort Lines Reversed | `edit.sortLinesReversed` | — | — |
| Remove Duplicate Lines | `edit.removeDuplicateLines` | — | — |
| Trim Trailing Whitespace | `edit.trimWhitespace` | — | — |
| Indent Using Spaces | `edit.indentUsingSpaces` | — | — |
| Indent Using Tabs | `edit.indentUsingTabs` | — | — |
| Detect Indentation from File | `edit.detectIndent` | — | — |
| Change Line Endings to LF | `edit.lineEndingsLf` | — | — |
| Change Line Endings to CRLF | `edit.lineEndingsCrlf` | — | — |

### Find in the file

| Title | Id | macOS | Windows and Linux |
|---|---|---|---|
| Find | `find.open` | `Cmd+F` | `Ctrl+F` |
| Replace | `find.replace` | `Cmd+Option+F` | `Ctrl+H` |
| Toggle Match Case *(find bar)* | `find.toggleCase` | `Option+C` | `Alt+C` |
| Toggle Regular Expression *(find bar)* | `find.toggleRegex` | `Option+R` | `Alt+R` |
| Replace All *(find bar)* | `find.replaceAll` | `Cmd+Enter` | `Ctrl+Enter` |

### Find in files

| Title | Id | macOS | Windows and Linux |
|---|---|---|---|
| Find in Files | `search.inFiles` | `Cmd+Shift+F` | `Ctrl+Shift+F` |
| Replace in Files | `search.replaceInFiles` | `Cmd+Shift+H` | `Ctrl+Shift+H` |
| Next Search Result | `search.next` | `F4` | `F4` |
| Previous Search Result | `search.previous` | `Shift+F4` | `Shift+F4` |
| Find in Files: Toggle Match Case | `search.toggleCase` | — | — |
| Find in Files: Toggle Regular Expression | `search.toggleRegex` | — | — |
| Find in Files: Toggle Whole Word | `search.toggleWord` | — | — |
| Replace in Files: Replace All | `search.replaceAll` | `Cmd+Option+Enter` | `Ctrl+Alt+Enter` |
| Undo Replace in Files | `search.undoReplace` | — | — |
| Clear Search Results | `search.clear` | — | — |
| Save Search… | `search.save` | — | — |

### View, groups and folding

| Title | Id | macOS | Windows and Linux |
|---|---|---|---|
| Toggle Soft Wrap | `view.toggleWrap` | `Option+Z` | `Alt+Z` |
| Go to Line… | `edit.goToLine` | `Cmd+G` | `Ctrl+G` |
| Go to Symbol… | `edit.goToSymbol` | `Cmd+Shift+R` | `Ctrl+Shift+R` |
| Follow Tail | `view.followTail` | — | — |
| Toggle Sidebar | `view.toggleSidebar` | `Cmd+B` | `Ctrl+B` |
| Split Right | `view.splitRight` | `Cmd+\` | `Ctrl+\` |
| Close Group | `view.closeGroup` | — | — |
| Focus Left Group | `view.focusLeftGroup` | `Cmd+Option+Left` | `Ctrl+Alt+Left` |
| Focus Right Group | `view.focusRightGroup` | `Cmd+Option+Right` | `Ctrl+Alt+Right` |
| Move Tab to Next Group | `tab.moveToNextGroup` | — | — |
| Fold | `fold.fold` | `Cmd+Option+[` | `Ctrl+Alt+[` |
| Unfold | `fold.unfold` | `Cmd+Option+]` | `Ctrl+Alt+]` |
| Toggle Fold | `fold.toggleAtCursor` | `Cmd+K Cmd+L` | `Ctrl+K Ctrl+L` |
| Fold All | `fold.foldAll` | `Cmd+K Cmd+0` | `Ctrl+K Ctrl+0` |
| Unfold All | `fold.unfoldAll` | `Cmd+K Cmd+J` | `Ctrl+K Ctrl+J` |
| Fold Level 1 | `fold.level1` | `Cmd+K Cmd+1` | `Ctrl+K Ctrl+1` |
| Fold Level 2 | `fold.level2` | `Cmd+K Cmd+2` | `Ctrl+K Ctrl+2` |
| Fold Level 3 | `fold.level3` | `Cmd+K Cmd+3` | `Ctrl+K Ctrl+3` |
| Focus Sidebar | `view.focusSidebar` | `Cmd+Shift+E` | `Ctrl+Shift+E` |
| Toggle Debug Overlay | `view.toggleDebugOverlay` | `F12` | `F12` |
| Zoom In | `view.zoomIn` | `Cmd+=` | `Ctrl+=` |
| Zoom Out | `view.zoomOut` | `Cmd+-` | `Ctrl+-` |
| Reset Zoom | `view.resetZoom` | `Cmd+0` | `Ctrl+0` |
| Toggle Render Whitespace (selection, all, none) | `view.toggleWhitespace` | — | — |
| Toggle Indent Guides | `view.toggleIndentGuides` | — | — |
| Toggle Sticky Scroll | `view.toggleStickyScroll` | — | — |
| Toggle Minimap | `view.toggleMinimap` | — | — |
| Open Preview (Markdown) | `view.openPreview` | `Cmd+Shift+V` | `Ctrl+Shift+V` |
| Show/Hide Ignored Files in Sidebar | `view.toggleIgnored` | — | — |
| Reload from Disk | `file.reloadFromDisk` | — | — |
| Keep My Version (ignore the change on disk) | `file.keepMine` | — | — |
| Toggle Light/Dark Theme | `view.toggleTheme` | — | — |

### Cursor movement and selection

| Title | Id | macOS | Windows and Linux |
|---|---|---|---|
| Cursor Left † | `cursor.left` | `Left` | `Left` |
| Cursor Right † | `cursor.right` | `Right` | `Right` |
| Cursor Up † | `cursor.up` | `Up` | `Up` |
| Cursor Down † | `cursor.down` | `Down` | `Down` |
| Cursor Word Left † | `cursor.wordLeft` | `Option+Left` | `Ctrl+Left` |
| Cursor Word Right † | `cursor.wordRight` | `Option+Right` | `Ctrl+Right` |
| Cursor to Line Start † | `cursor.lineStart` | `Home`, `Cmd+Left` | `Home` |
| Cursor to Line End † | `cursor.lineEnd` | `End`, `Cmd+Right` | `End` |
| Page Up † | `cursor.pageUp` | `PageUp` | `PageUp` |
| Page Down † | `cursor.pageDown` | `PageDown` | `PageDown` |
| Cursor to Start of File † | `cursor.docStart` | `Cmd+Up`, `Cmd+Home`, `Ctrl+Home` | `Ctrl+Home`, `Ctrl+Up` |
| Cursor to End of File † | `cursor.docEnd` | `Cmd+Down`, `Cmd+End`, `Ctrl+End` | `Ctrl+End`, `Ctrl+Down` |
| Select Left † | `select.left` | `Shift+Left` | `Shift+Left` |
| Select Right † | `select.right` | `Shift+Right` | `Shift+Right` |
| Select Up † | `select.up` | `Shift+Up` | `Shift+Up` |
| Select Down † | `select.down` | `Shift+Down` | `Shift+Down` |
| Select Word Left † | `select.wordLeft` | `Option+Shift+Left` | `Ctrl+Shift+Left` |
| Select Word Right † | `select.wordRight` | `Option+Shift+Right` | `Ctrl+Shift+Right` |
| Select to Line Start † | `select.lineStart` | `Shift+Home`, `Cmd+Shift+Left` | `Shift+Home` |
| Select to Line End † | `select.lineEnd` | `Shift+End`, `Cmd+Shift+Right` | `Shift+End` |
| Select Page Up † | `select.pageUp` | `Shift+PageUp` | `Shift+PageUp` |
| Select Page Down † | `select.pageDown` | `Shift+PageDown` | `Shift+PageDown` |
| Select to Start of File † | `select.docStart` | `Cmd+Shift+Up`, `Cmd+Shift+Home` | `Ctrl+Shift+Home` |
| Select to End of File † | `select.docEnd` | `Cmd+Shift+Down`, `Cmd+Shift+End` | `Ctrl+Shift+End` |
