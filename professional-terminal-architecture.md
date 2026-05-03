# Professional Terminal Emulator Architecture Reference

A comprehensive analysis of Terminator (Python/GTK) and iTerm2 (Swift/ObjC) internals, with architectural recommendations for the Rustinator project.

---

## 1. Executive Summary

Both Terminator and iTerm2 have evolved over many years into stable, production-grade terminal emulators. Despite being written in vastly different languages and targeting different platforms, they converge on several key architectural principles:

**Where they agree:**
- A strict tree-based hierarchy for managing splits and panes, with each node owning exactly zero or two children (binary split tree).
- Clear separation between terminal emulation state and the visual rendering of that state.
- A centralized singleton or controller that tracks all sessions/terminals globally and manages lifecycle events.
- Layout serialization as flat dictionaries with parent references, reconstructed into trees at restore time.
- Configuration layered with defaults, profiles, and per-session overrides, propagated via explicit reconfigure sweeps rather than live binding.
- Delegate/signal patterns to decouple components rather than direct method calls across layers.

**Where they diverge:**
- iTerm2 has a dedicated multithreaded parsing and token execution pipeline (parser thread plus main thread sync), while Terminator offloads parsing entirely to the VTE library.
- iTerm2 owns its own screen buffer, line buffer, and grid abstractions. Terminator delegates all of this to the VTE widget.
- iTerm2 has a GPU-accelerated Metal rendering path. Terminator relies on VTE's Cairo-based rendering.
- iTerm2's codebase is roughly 50x larger and handles far more edge cases (tmux integration, SSH conductor, instant replay, scripting API).

**Key takeaway for Rustinator:** The most critical architectural decision is the clean separation of four layers: PTY management, terminal emulation/state, screen buffer, and rendering. Both projects suffer (or suffered) when these layers bleed into each other. Terminator avoids the problem by using VTE as a monolithic black box. iTerm2 owns everything and pays the complexity cost but gains total control. Rustinator, being in Rust, should own the full stack but enforce layer boundaries through traits and module visibility.

---

## 2. Terminator Deep Dive

### 2.1 Object Hierarchy

Terminator's object model forms a tree:

```
Terminator (singleton, application-level coordinator)
  |
  +-- Window (Gtk.Window + Container)
        |
        +-- [one of:]
             +-- Terminal (leaf: Gtk.VBox wrapping a VTE widget)
             +-- HPaned / VPaned (binary split container)
             |     +-- child1: Terminal or HPaned/VPaned
             |     +-- child2: Terminal or HPaned/VPaned
             +-- Notebook (tabbed container)
                   +-- page0: Terminal or HPaned/VPaned
                   +-- page1: Terminal or HPaned/VPaned
                   +-- ...
```

**Key classes and their files:**

| Class | File | Role |
|-------|------|------|
| `Terminator` | `terminatorlib/terminator.py` | Application singleton. Tracks all windows, terminals, groups. |
| `Window` | `terminatorlib/window.py` | Top-level GTK window. Inherits from both `Container` and `Gtk.Window`. |
| `Container` | `terminatorlib/container.py` | Abstract base for anything that holds children. Defines `split_axis()`, `add()`, `remove()`, `closeterm()`. |
| `Paned` / `HPaned` / `VPaned` | `terminatorlib/paned.py` | Binary split containers. Inherit from both `Container` and `Gtk.HPaned`/`Gtk.VPaned`. |
| `Notebook` | `terminatorlib/notebook.py` | Tab container. Inherits from both `Container` and `Gtk.Notebook`. |
| `Terminal` | `terminatorlib/terminal.py` | Leaf node. A `Gtk.VBox` containing a `Vte.Terminal` widget, a titlebar, a scrollbar, and a search bar. |
| `Factory` | `terminatorlib/factory.py` | Object creation factory. Borg singleton. |
| `Config` / `ConfigBase` | `terminatorlib/config.py` | Layered configuration system. |
| `Signalman` | `terminatorlib/signalman.py` | Signal connection tracker for clean disconnection. |

### 2.2 The Container/Split Model

The split tree is Terminator's most important data structure. Every split operation follows this algorithm (from `paned.py:split_axis`):

1. Remove the target widget from its parent.
2. Create a new `HPaned` or `VPaned`.
3. Add the new paned container to the parent (in the spot the widget occupied).
4. Create a new `Terminal` (the sibling).
5. Add both the original widget and the sibling as children of the paned container.

Closing a terminal reverses this: the `Paned.wrapcloseterm()` method detects that only one child remains after closing, then replaces itself in the parent with that surviving child. This is the `hoover()` pattern -- containers check whether they still have a reason to exist and self-destruct if not.

The ratio-based positioning system (`self.ratio`) stores split positions as floating-point ratios (0.0 to 1.0) rather than pixel positions, which enables layouts to survive window resizes. The `set_pos_by_ratio` flag on the window coordinates resize events so positions update proportionally.

### 2.3 The Signal/Event System

Terminator uses GTK's GObject signal system extensively. The `Terminal` class declares ~30 custom signals (`__gsignals__` in `terminal.py`):

```python
'close-term', 'title-change', 'split-auto', 'split-horiz', 'split-vert',
'resize-term', 'navigate', 'tab-change', 'group-all', 'zoom', 'maximise',
'rotate-cw', 'rotate-ccw', 'tab-new', 'move-tab', ...
```

When a terminal is added to a container, the container connects handlers for these signals. For example, when a `Terminal` is added to a `Paned` (`paned.py:add()`), the paned connects handlers like:

```python
signals = {
    'close-term': self.wrapcloseterm,
    'split-horiz': self.split_horiz,
    'split-vert': self.split_vert,
    'title-change': self.propagate_title_change,
    'resize-term': self.resizeterm,
    'zoom': top_window.zoom,
    'tab-change': top_window.tab_change,
    'navigate': top_window.navigate_terminal,
    ...
}
```

Notice that some signals are handled locally (split, resize) while others are forwarded up to the `Window` (zoom, tab-change, navigate). This creates a chain-of-responsibility pattern where events bubble up through the container tree until something handles them.

The `Signalman` class (`signalman.py`) is a bookkeeping utility that tracks signal connections per widget, enabling clean disconnection when widgets are removed from the tree. Without it, stale signal handlers would fire on destroyed widgets.

**Critical observation:** The `resize-term` signal demonstrates how unhandled events propagate. When a `VPaned` receives a resize request for "left" or "right" and it can only handle "up"/"down", it re-emits the signal upward. This is clean but can cause unexpected cascading if the tree is deep.

### 2.4 The Borg Pattern

The Borg pattern (`borg.py`) is Terminator's alternative to the Singleton pattern. Instead of ensuring only one instance of a class exists, Borg ensures all instances share the same state:

```python
class Borg:
    __shared_state = {}
    def __init__(self, borgtype=None):
        if borgtype not in self.__shared_state:
            self.__shared_state[borgtype] = {}
        self.__dict__ = self.__shared_state[borgtype]
```

By replacing `self.__dict__` with a shared dictionary keyed by class name, every instance of a Borg subclass sees the same attributes. This is used for:

- `Terminator` -- the application coordinator
- `ConfigBase` -- the configuration store
- `Factory` -- the object factory
- `PluginRegistry` -- the plugin system

**Why Borg instead of Singleton:** Borg avoids the complex metaclass machinery that Python singletons require. It also plays better with inheritance -- subclasses get their own shared state bucket. The trade-off is that it is slightly confusing: `Terminator()` looks like it creates a new object, but it is actually just accessing the shared state.

**Rust equivalent:** In Rust, this pattern maps naturally to `Arc<Mutex<AppState>>` or similar shared state behind a global accessor. The Borg pattern's key insight -- that shared state and object identity are separate concerns -- is actually enforced by Rust's ownership system.

### 2.5 The Factory Pattern

The `Factory` class (`factory.py`) centralizes object creation. It maps type names to module names:

```python
types = {
    'Terminal': 'terminal',
    'VPaned': 'paned',
    'HPaned': 'paned',
    'Notebook': 'notebook',
    'Window': 'window'
}
```

This is used extensively during layout restoration, where the type of object to create comes from a serialized string. The factory also provides `isinstance()` checks that work across Terminator's non-standard inheritance hierarchy (multiple inheritance from both `Container` and GTK widget classes).

### 2.6 Configuration Propagation

Configuration in Terminator is layered:

1. **DEFAULTS** -- hardcoded default values in `config.py` (global config, keybindings, profiles, layouts, plugins).
2. **ConfigBase** (Borg) -- loads and merges config file values over defaults.
3. **Config** -- a profile-aware wrapper. Each `Terminal` gets its own `Config` instance pointing to a specific profile.

When configuration changes (e.g., a user changes the font in preferences), the `Terminator.reconfigure()` method is called, which:

1. Rebuilds CSS style providers.
2. Iterates over ALL terminals calling `terminal.reconfigure()`.
3. Re-parses keybindings.
4. Updates notebook tab positions for all windows.

This is a **push-based full sweep** -- every reconfigure touches every terminal. There is no fine-grained change notification. The `Config.__getitem__` method uses a lookup chain: global config -> active profile -> keybindings section, which means profile-specific settings override global settings transparently.

### 2.7 Layout Serialization and Restoration

Layout serialization (`container.py:describe_layout()`) walks the tree depth-first and produces a flat dictionary:

```python
{
    'child0': {'type': 'Window', 'parent': '', 'position': '100:200', 'size': [800, 600], ...},
    'child1': {'type': 'VPaned', 'parent': 'child0', 'ratio': 0.5, 'order': 0},
    'child2': {'type': 'Terminal', 'parent': 'child1', 'order': 0, 'profile': 'default', ...},
    'child3': {'type': 'Terminal', 'parent': 'child1', 'order': 1, 'profile': 'default', ...}
}
```

Restoration (`terminator.py:create_layout()`) reconstructs the tree by:

1. Iterating through the flat dictionary in a loop (up to 1000 iterations as a safety bound).
2. Processing Window objects first (they have no parent).
3. Then processing children whose parents have already been created.
4. Building a hierarchy dictionary.
5. Walking the hierarchy to create actual Widget objects via the Factory.

Each container type has a `create_layout()` method that recursively instantiates its children. Terminal spawning is deferred until `layout_done()` to avoid race conditions where terminals start outputting before the UI is fully assembled.

**Key detail:** Split ratios are stored rather than pixel positions, and `set_pos_by_ratio` is toggled during layout restoration so that position changes during widget realization do not clobber the intended ratios.

### 2.8 PTY/Session Management

Terminator delegates almost everything PTY-related to the VTE library. The `Terminal.spawn_child()` method (not fully shown but present in terminal.py) calls `self.vte.spawn_async()` or `self.vte.spawn_sync()`, which:

1. Forks a child process.
2. Sets up a PTY pair.
3. Starts a shell or custom command.
4. Begins reading output and feeding it through the VTE terminal emulator.

The `Terminal` class tracks only the PID (`self.pid`) and watches for the `child-exited` signal to determine what to do (close, restart, or hold open depending on configuration).

### 2.9 Plugin System

The plugin system (`plugin.py`) is simple and effective:

1. `PluginRegistry` (Borg) scans `plugins/` directories for `.py` files.
2. Each plugin module exports an `AVAILABLE` list of class names.
3. Plugin classes inherit from base classes like `URLHandler` or `MenuItem` that declare `capabilities`.
4. At runtime, `get_plugins_by_capability()` returns all plugins matching a requested capability.

Plugins can:
- Add URL regex handlers to all terminals.
- Add items to the terminal context menu.
- React to terminal lifecycle events.

The registry respects `enabled_plugins` from config and supports load/unload cycles.

---

## 3. iTerm2 Deep Dive

### 3.1 The Session Model

`PTYSession` (`sources/PTYSession/PTYSession.h`) is the central object in iTerm2. It is the **model** for one terminal pane. A session owns:

- `PTYTask *shell` -- the PTY/process wrapper
- `VT100Screen *screen` -- the terminal screen state (buffer, grid, scrollback)
- `VT100Terminal *terminal` (via screen) -- the escape code parser/state machine
- `PTYTextView *textview` -- the rendering view
- `SessionView *view` -- the container view that holds the text view and chrome

The session implements a staggering number of delegate protocols:

```objc
@interface PTYSession : NSResponder <
    PTYTaskDelegate,        // PTY I/O events
    PTYTextViewDelegate,    // User interaction events  
    VT100ScreenDelegate,    // Screen state changes
    TmuxGatewayDelegate,    // Tmux protocol
    iTermFindDriverDelegate,// Search
    ...>
```

This makes `PTYSession` the central hub through which all events flow. The delegate pattern is used instead of direct method calls, which creates clean dependency inversion: the screen does not know about the session, the session does not know about the tab, the tab does not know about the window.

### 3.2 The VT100 Parsing Pipeline

iTerm2 has a sophisticated multi-stage parsing pipeline:

```
Raw bytes from PTY
    |
    v
VT100Parser  (sources/VT100/VT100Parser.h)
    |  - putStreamData: accumulates raw bytes
    |  - addParsedTokensToVector: parses into VT100Token objects
    |  - Delegates to specialized sub-parsers:
    |      VT100CSIParser   (CSI sequences like cursor movement)
    |      VT100AnsiParser  (ANSI escape sequences)
    |      VT100DCSParser   (Device Control Strings)
    |      VT100XtermParser (xterm-specific OSC sequences)
    |      VT100ControlParser (C0/C1 control characters)
    |      VT100StringParser (plain text runs)
    v
VT100Token (sources/VT100/VT100Token.h)
    |  - A tagged union representing one parsed element
    |  - Type field: VT100_STRING, VT100CSI, ESC, XTERMCC, etc.
    v
TokenExecutor (sources/VT100/TokenExecutor.swift)
    |  - Runs on a dedicated execution queue
    |  - Batches tokens for efficiency
    |  - Supports pause/unpause (for copy mode, coprocesses)
    |  - Calls back to delegate for synchronization
    v
VT100Terminal.executeToken: (sources/VT100/VT100Terminal.m)
    |  - Giant switch on token type
    |  - Translates tokens into delegate method calls
    |  - VT100Terminal is "dumb" -- it does not modify screen state directly
    v
VT100TerminalDelegate (implemented by VT100ScreenMutableState)
    |  - terminalAppendString:, terminalCursorLeft:, etc.
    |  - Actually modifies the screen buffer
    v
VT100ScreenDelegate (implemented by PTYSession)
    - screenNeedsRedraw, screenSetWindowTitle:, etc.
    - Bridges screen state changes to UI updates
```

**Critical architectural insight:** The parsing happens on a background thread. The `TokenExecutor` manages the boundary between the parser thread and the main thread. The `VT100ScreenMutableState` processes tokens on the mutation thread, and periodically "syncs" with the main thread's `VT100ScreenState` via the `synchronizeWithConfig:` method. This is iTerm2's most complex subsystem and the source of many subtle threading bugs that were fixed over the years.

### 3.3 Screen Buffer Management

The screen buffer has several layers:

1. **`screen_char_t`** (`sources/ScreenChar/ScreenChar.h`) -- the fundamental cell type. A packed struct containing a character code (or index into a string table for complex characters), foreground/background color codes, and attribute flags (bold, italic, underline, etc.). Special sentinel values like `DWC_SKIP`, `TAB_FILLER`, and `DWC_RIGHT` handle double-width characters and tab stops.

2. **`VT100Grid`** (`sources/VT100/VT100Grid.h`) -- a 2D grid of `screen_char_t` cells with cursor position, scroll regions, and dirty tracking. The grid supports:
   - Per-line dirty range tracking (for efficient redraw)
   - Scroll regions (top/bottom and left/right margins)
   - Line attributes (double-width, double-height)

3. **`LineBuffer`** (`sources/LineBuffer/LineBuffer.h`) -- the scrollback buffer. Implemented as an array of `LineBlock` objects. LineBuffer stores raw character data and can reflow it to any width on demand. Key features:
   - Lines are stored as variable-length raw data, not fixed-width grids.
   - Reflowing to a new width is done lazily.
   - The `LineBufferPosition` type enables stable references to positions that survive reflow.
   - Dropped lines (when scrollback limit is reached) are tracked by `numberOfDroppedChars`.

4. **`VT100Screen`** (`sources/VT100Screen/VT100Screen.h`) -- ties everything together. Owns:
   - A primary grid (normal mode)
   - An alternate grid (alternate screen mode)
   - A LineBuffer for scrollback
   - A VT100Terminal for parsing
   - An interval tree for marks, annotations, and other metadata attached to line ranges.

The screen has a split state model:
- **`VT100ScreenMutableState`** -- the state that the mutation thread modifies.
- **`VT100ScreenState`** -- an immutable snapshot that the main thread reads for rendering.

Synchronization happens via `synchronizeWithConfig:` which atomically swaps the mutable state changes into the shared state.

### 3.4 The Split/Pane Model

iTerm2's pane management hierarchy:

```
iTermController (singleton)
    |
    +-- PseudoTerminal (window controller, sources/TerminalView/PseudoTerminal.h)
          |
          +-- PTYTabView (tab bar)
                |
                +-- PTYTab (sources/TerminalView/PTYTab.h)
                      |
                      +-- NSSplitView (root split view, sources/TerminalView/PTYSplitView.h)
                            |
                            +-- SessionView
                            |     +-- PTYSession (the terminal)
                            +-- NSSplitView (nested)
                                  +-- SessionView
                                  +-- SessionView
```

`PTYTab` is the key class. It:
- Owns the root `NSSplitView`.
- Maintains a map from `SessionView` to `PTYSession`.
- Implements `NSSplitViewDelegate` to control split behavior.
- Implements `PTYSessionDelegate` -- every session in the tab delegates to the tab.
- Handles maximization (zooming one pane to fill the tab) by hiding all other session views.
- Provides ordered session traversal for keyboard navigation.

Unlike Terminator's custom `HPaned`/`VPaned` classes, iTerm2 uses the native `NSSplitView` directly, with `PTYSplitView` as a thin subclass that adds drag-end notifications and a unique identifier.

Layout serialization in iTerm2 uses "arrangements" -- nested dictionaries that capture the complete state of a window, its tabs, their split configurations, and individual session states. The restoration path uses `+tabWithArrangement:` and `+sessionFromArrangement:` class methods, which is similar in spirit to Terminator's approach but includes far more state (scrollback contents, marks, selection, etc.).

### 3.5 The Rendering Pipeline

iTerm2 has two rendering paths:

**Legacy path (CoreText/AppKit):**
- `PTYTextView` (`sources/TerminalView/PTYTextView.h`) is an `NSView` subclass.
- `iTermTextDrawingHelper` (`sources/Drawing/iTermTextDrawingHelper.h`) does the heavy lifting.
- Uses CoreText for text layout and `iTermAttributedStringBuilder` for constructing attributed strings from screen characters.
- Renders backgrounds, text, cursors, selections, and indicators in separate passes.

**Metal path (GPU-accelerated):**
- `iTermMetalDriver` (`sources/MetalRenderer/iTermMetalDriver.h`) orchestrates Metal rendering.
- Uses `iTermMetalView` as the `MTKView` subclass.
- The driver gets per-frame state from `iTermMetalDriverDataSourcePerFrameState`, which is a snapshot protocol that the text view conforms to.
- Separate renderer objects handle different visual elements: `iTermTextRenderer`, `iTermBackgroundColorRenderer`, `iTermCursorRenderer`, `iTermImageRenderer`, `iTermIndicatorRenderer`, `iTermMarkRenderer`.
- Glyph rendering uses a texture atlas (`iTermASCIITexture` for ASCII, dynamic atlas for non-ASCII).
- The Metal path draws asynchronously: the driver captures a frame's state, submits GPU work, and calls a completion block when done.

**Key architectural decision:** The rendering layer never reads screen state directly. Instead, it reads from a per-frame state snapshot. This means the screen can continue being modified by incoming data while the previous frame is being rendered. The `iTermMetalDriverDataSourcePerFrameState` protocol is the clean abstraction boundary.

### 3.6 Session Creation and Wiring

`iTermSessionFactory` (`sources/SessionCreation/iTermSessionFactory.h`) manages creating and launching sessions:

1. `newSessionWithProfile:parent:` creates a `PTYSession` from a profile dictionary.
2. `attachOrLaunchWithRequest:` handles the complex process of either attaching to an existing server process (for session restoration) or launching a new child process.

The launch request object (`iTermSessionAttachOrLaunchRequest`) encapsulates all the parameters needed, which is a good pattern for avoiding method signatures with 15+ parameters.

Session wiring follows this sequence:
1. Create `PTYSession`.
2. Set screen size and assign to a `SessionView`.
3. Load profile preferences into the session.
4. Start the program (fork + exec via `PTYTask`).
5. The task reads from the PTY fd and feeds data to `VT100Screen.threadedReadTask:length:`.
6. The screen's parser processes the data into tokens.
7. Tokens are executed, modifying screen state.
8. The screen signals the delegate (session) that a redraw is needed.
9. The session tells the text view to refresh.

### 3.7 Shell Integration

Shell integration (`sources/ShellIntegration/`) allows iTerm2 to understand shell semantics:

- Where prompts begin and end.
- What command was typed.
- What the return code was.
- What the current working directory is.
- What the current hostname is.

This is implemented via special escape sequences (OSC codes) that the shell emits. The `VT100Screen` processes these and creates `VT100ScreenMark` objects stored in an interval tree, anchored to absolute line numbers. The session uses this information for:

- Command history tracking (`iTermShellHistoryController`)
- Directory tracking
- Return code display in the gutter
- Automatic profile switching based on hostname

---

## 4. Comparative Analysis

### 4.1 Ownership of Terminal Emulation

| Aspect | Terminator | iTerm2 |
|--------|-----------|--------|
| VT100 parsing | Delegated to VTE library | Owned: VT100Parser + sub-parsers |
| Screen buffer | Delegated to VTE library | Owned: VT100Grid + LineBuffer |
| Text rendering | Delegated to VTE library | Owned: CoreText + Metal renderers |
| PTY management | VTE's spawn_async/spawn_sync | Owned: PTYTask |

Terminator is essentially a UI shell around the VTE widget. iTerm2 owns the entire stack.

**Implication for Rustinator:** You are already building your own VT100 parser and screen buffer (based on the existing source files). This is the right call for a Rust project where you want total control. However, you must enforce the layer boundaries that iTerm2's delegate protocols provide.

### 4.2 Threading Models

| Aspect | Terminator | iTerm2 |
|--------|-----------|--------|
| UI thread | GTK main loop (single-threaded) | macOS main thread |
| Parsing | VTE handles internally | Dedicated parser/mutation thread |
| Rendering | GTK/Cairo on main thread | Metal async rendering on GPU thread |
| I/O | VTE handles internally | PTYTask reads on background thread |

iTerm2's threading model is significantly more complex but enables higher performance. The key innovation is the `VT100ScreenMutableState` / `VT100ScreenState` split, which allows parsing to proceed without blocking the UI.

### 4.3 Event Flow

**Terminator:** Terminal emits GObject signal -> Container handles or re-emits -> Window handles.

**iTerm2:** Screen calls delegate method on PTYSession -> Session calls delegate method on PTYTab -> Tab calls method on PseudoTerminal.

Both use an upward-propagating delegation chain, but iTerm2's is more formalized through protocol conformance, while Terminator's is more ad-hoc through signal name matching.

### 4.4 Configuration

**Terminator:** Borg-based Config with profile overlay. Full-sweep reconfigure on change.

**iTerm2:** Profile dictionaries with "divorced" profiles (per-session overrides). Changes detected through KVO and delegate callbacks. More granular but also more complex.

---

## 5. Recommended Architecture for Rustinator

### 5.1 Layer Diagram

```
+-------------------------------------------------------------------+
|                        Application Layer                          |
|  AppState, Config, Keybindings, Plugin System                     |
+-------------------------------------------------------------------+
|                        Window/Layout Layer                         |
|  Window, SplitTree (arena-based), TabBar                         |
+-------------------------------------------------------------------+
|                        Session Layer                               |
|  Session (owns one Pty + one TerminalState + one Renderer slot)  |
+-------------------------------------------------------------------+
|                     Terminal Emulation Layer                       |
|  VT100Parser, TokenStream, TerminalState (grid + scrollback)    |
+-------------------------------------------------------------------+
|                        PTY Layer                                  |
|  PtyProcess (fork/exec, fd management, async I/O)               |
+-------------------------------------------------------------------+
|                       Rendering Layer                              |
|  Trait: TerminalRenderer                                          |
|  Impls: MetalRenderer (macOS), OpenGLRenderer (Linux)            |
+-------------------------------------------------------------------+
```

### 5.2 The Split Tree (Arena-Based)

Use an arena allocator (e.g., `slotmap` or `generational-arena`) to store the split tree. This avoids the reference-counting complexity that both Terminator and iTerm2 struggle with.

```rust
pub enum SplitNode {
    Leaf {
        session_id: SessionId,
    },
    Split {
        direction: SplitDirection,
        ratio: f64,
        first: NodeId,
        second: NodeId,
    },
}

pub enum SplitDirection {
    Horizontal,
    Vertical,
}

pub struct SplitTree {
    arena: Arena<SplitNode>,
    root: NodeId,
}
```

Operations on the tree:
- `split(node_id, direction)` -- replace a leaf with a split containing the original and a new leaf.
- `close(node_id)` -- remove a leaf, promote the sibling (Terminator's "hoover" pattern).
- `describe_layout()` -- serialize to a flat structure for saving.
- `create_layout(layout)` -- deserialize and rebuild.

This is cleaner than Terminator's approach (which conflates the tree structure with GTK widget parentage) and avoids the need for iTerm2's `NSSplitView` delegation complexity.

### 5.3 Separating PTY from Emulation from Rendering

Define hard boundaries with traits:

```rust
/// The PTY layer only knows about bytes.
pub trait PtyBackend {
    fn spawn(config: &PtyConfig) -> Result<Self, PtyError>;
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, PtyError>;
    fn write(&self, data: &[u8]) -> Result<usize, PtyError>;
    fn resize(&self, cols: u16, rows: u16) -> Result<(), PtyError>;
    fn pid(&self) -> u32;
}

/// The emulator processes bytes and updates terminal state.
/// It never touches the PTY or the renderer.
pub trait TerminalEmulator {
    fn process_bytes(&mut self, input: &[u8]);
    fn state(&self) -> &TerminalState;
}

/// The renderer reads terminal state and produces pixels.
/// It never modifies terminal state or touches the PTY.
pub trait TerminalRenderer {
    fn render(&mut self, state: &TerminalState, viewport: &Viewport);
    fn resize_viewport(&mut self, width: u32, height: u32);
}
```

**The Session owns all three and coordinates them:**

```rust
pub struct Session {
    pty: Box<dyn PtyBackend>,
    emulator: Box<dyn TerminalEmulator>,
    renderer: Box<dyn TerminalRenderer>,
    config: SessionConfig,
}
```

This enforces the separation that iTerm2 achieves through delegate protocols and that Terminator never needs (because VTE is a monolith).

### 5.4 Event/Signal Architecture

Use a typed event enum and channel-based dispatch rather than string-based signals (Terminator) or protocol methods (iTerm2):

```rust
pub enum SessionEvent {
    TitleChanged(String),
    BellRang,
    ChildExited(i32),
    OutputReceived,
    RequestSplit { direction: SplitDirection },
    RequestClose,
    RequestNavigate { direction: NavigationDirection },
    RequestResize { cols: u16, rows: u16 },
}

pub enum AppEvent {
    SessionEvent { session_id: SessionId, event: SessionEvent },
    ConfigChanged,
    WindowClosed { window_id: WindowId },
}
```

Route all events through a central event loop (not unlike GTK's main loop or macOS's run loop). This prevents the cascading-signal problem that Terminator has, where a resize signal bounces up through multiple pane levels.

The key principle from both codebases: **events flow upward (child to parent), commands flow downward (parent to child).** A terminal emits a "request split" event. The split tree handler receives it and performs the split. The terminal never modifies the tree directly.

### 5.5 Screen Buffer Design

Based on iTerm2's proven architecture:

```rust
/// A single cell in the terminal grid.
#[derive(Clone, Copy)]
pub struct Cell {
    pub character: char,  // Or a compact representation with a string table
    pub fg: ColorCode,
    pub bg: ColorCode,
    pub attrs: CellAttributes,  // Bold, italic, underline, etc. as bitflags
}

/// A fixed-size grid representing the visible terminal area.
pub struct Grid {
    cells: Vec<Cell>,     // Row-major, width * height cells
    width: usize,
    height: usize,
    cursor: CursorPosition,
    scroll_region: Option<ScrollRegion>,
    dirty_lines: BitVec,  // Track which lines need redraw
}

/// The scrollback buffer, stored as variable-length lines.
pub struct ScrollbackBuffer {
    lines: VecDeque<ScrollbackLine>,
    max_lines: usize,
    dropped_count: u64,
}

/// Complete terminal state.
pub struct TerminalState {
    primary_grid: Grid,
    alternate_grid: Grid,
    active_grid: GridSelector,  // Which grid is currently active
    scrollback: ScrollbackBuffer,
    // ... modes, character sets, saved cursor, etc.
}
```

Key design decisions:
- **Dirty line tracking** (from iTerm2) is essential for performance. Only redraw lines that changed.
- **Separate primary and alternate grids** (both codebases do this). The alternate screen is used by full-screen applications like vim.
- **Variable-length scrollback lines** (from iTerm2's LineBuffer) save memory compared to fixed-width rows.
- **Generation counter** on the buffer for change detection (from iTerm2's `generation` property on LineBuffer).

### 5.6 Making the Renderer Swappable

The `TerminalRenderer` trait from section 5.3 is the abstraction boundary. Implementations:

```rust
#[cfg(target_os = "macos")]
pub struct MetalRenderer { /* ... */ }

#[cfg(target_os = "linux")]
pub struct OpenGLRenderer { /* ... */ }
```

The renderer should receive a **snapshot** of the terminal state, not a live reference. This is iTerm2's `iTermMetalDriverDataSourcePerFrameState` pattern:

```rust
pub struct RenderSnapshot {
    pub grid: GridSnapshot,      // Immutable copy of visible cells
    pub cursor: CursorInfo,
    pub selection: Option<SelectionRange>,
    pub dirty_lines: Vec<usize>, // Which lines changed since last frame
    pub scrollback_offset: usize,
}
```

Creating a snapshot is cheap if dirty tracking is used -- only copy the lines that changed. The renderer holds the previous frame's data and diffs against the new snapshot.

### 5.7 Configuration Propagation

Adopt a hybrid of both approaches:

1. **Layered config** (from Terminator): defaults -> user config -> profile -> session overrides.
2. **Change notification** (from iTerm2): when config changes, emit a `ConfigChanged` event.
3. **Selective reconfigure**: sessions subscribe to the config keys they care about and only reconfigure when relevant keys change.

```rust
pub struct Config {
    global: HashMap<String, ConfigValue>,
    profiles: HashMap<String, ProfileConfig>,
}

impl Config {
    pub fn get<T>(&self, key: &str, profile: &str) -> T {
        // Check profile first, then global, then default
    }
    
    pub fn set(&mut self, key: &str, value: ConfigValue, profile: Option<&str>) {
        // Set value and emit ConfigChanged event with the changed key
    }
}
```

---

## 6. Anti-Patterns to Avoid

### 6.1 The God Object

`PTYSession` in iTerm2 is over 10,000 lines and implements 10+ delegate protocols. It is the single point of contact for PTY events, screen events, UI events, tmux events, scripting events, and more. This makes it extremely difficult to modify one concern without risking another.

**Recommendation:** Keep the Session struct lean. Delegate specific concerns to sub-objects:
- `SessionPty` for PTY lifecycle
- `SessionEmulator` for terminal state
- `SessionRenderer` for display
- `SessionConfig` for profile/settings

### 6.2 String-Based Type Dispatch

Terminator's `Factory.isinstance()` and `Factory.type()` use string-based type checking throughout the codebase:

```python
if maker.isinstance(child, 'Terminal'):
    ...
elif maker.isinstance(child, 'Container'):
    ...
```

This is fragile (typos are silent failures) and opaque to static analysis.

**Recommendation:** Use Rust's enums and pattern matching. The split tree enum (`SplitNode::Leaf` vs `SplitNode::Split`) eliminates this entire class of bugs.

### 6.3 Shared Mutable State via Singletons/Borgs

Both codebases use global singletons heavily (`Terminator()` in Terminator, `iTermController.sharedInstance` in iTerm2). This makes testing difficult and creates hidden coupling.

**Recommendation:** Pass `AppState` explicitly or use dependency injection. If a global is truly needed, use `once_cell::sync::Lazy<Mutex<T>>` and document why.

### 6.4 Conflating Widget Tree with Logical Tree

Terminator's container hierarchy IS the GTK widget hierarchy. This means layout logic is tangled with widget lifecycle, signal connection, and rendering. When you split a terminal, you are simultaneously modifying the data model and the UI.

**Recommendation:** Keep the split tree as a pure data structure (the arena-based `SplitTree`). A separate reconciliation step maps the logical tree to the actual UI widget tree. This is analogous to React's virtual DOM or SwiftUI's declarative model, and it is what allows the renderer to be swappable.

### 6.5 Blocking the Main Thread During Layout

Both codebases have code that processes pending events in a loop during layout operations:

```python
# Terminator
while Gtk.events_pending():
    Gtk.main_iteration_do(False)
```

This is a code smell indicating that layout operations depend on GTK having processed size allocations. It creates subtle timing bugs.

**Recommendation:** Design layout operations to be purely computational (calculate positions and sizes from the tree and the available space) without depending on the UI toolkit's layout engine having run. Apply the computed layout to widgets in a single pass.

### 6.6 Leaking Threading Complexity into Business Logic

iTerm2's `performBlockWithJoinedThreads:` and `mutateAsynchronously:` patterns are necessary but leak threading concerns into every caller. Screen state access requires knowing which thread you are on and using the right synchronization method.

**Recommendation:** Encapsulate the threading model. The emulator runs on its own thread. It writes to an atomic snapshot. The renderer reads the snapshot. The session mediates via message passing (channels). No component outside the session needs to know about threads.

### 6.7 Over-Delegation

iTerm2's `VT100ScreenDelegate` protocol has over 100 methods. `VT100TerminalDelegate` has over 80. When a delegate protocol is this large, it means the delegating object is not sufficiently decomposed.

**Recommendation:** Use smaller, focused traits. Instead of one `ScreenDelegate` with 100 methods, have:
- `ScreenRedrawDelegate` (needs redraw, schedule redraw)
- `ScreenTitleDelegate` (title changed, icon changed)
- `ScreenBellDelegate` (bell rang)
- `ScreenResizeDelegate` (size changed)

Or better yet, use the event enum approach from section 5.4.

### 6.8 Not Tracking Dirty State

Terminator has no dirty tracking at all (VTE handles it internally). Any approach that redraws the entire screen on every change will be too slow for a modern terminal emulator processing megabytes per second of output.

**Recommendation:** Track dirty state at every level:
- Per-line dirty bits in the grid (from iTerm2).
- A generation counter on the scrollback buffer.
- A "needs redraw" flag on the session that the event loop checks.
- Incremental rendering that only updates changed texture regions.

---

## Appendix: File Reference

### Terminator Key Files
- `terminatorlib/terminator.py` -- Application singleton, window/terminal registry
- `terminatorlib/container.py` -- Base container class with split/close/layout methods
- `terminatorlib/paned.py` -- Binary split containers (HPaned, VPaned)
- `terminatorlib/window.py` -- Top-level window, event routing
- `terminatorlib/notebook.py` -- Tab container
- `terminatorlib/terminal.py` -- Terminal leaf node, VTE wrapper
- `terminatorlib/config.py` -- Configuration system with defaults, profiles, layouts
- `terminatorlib/factory.py` -- Object creation factory
- `terminatorlib/borg.py` -- Shared-state singleton pattern
- `terminatorlib/signalman.py` -- Signal connection tracker
- `terminatorlib/plugin.py` -- Plugin system with capabilities

### iTerm2 Key Files
- `sources/PTYSession/PTYSession.h` -- Session model (the hub)
- `sources/VT100/VT100Terminal.h` -- Escape code parser/state machine
- `sources/VT100/VT100Parser.h` -- Raw byte to token parser
- `sources/VT100/TokenExecutor.swift` -- Threaded token execution
- `sources/VT100/VT100Grid.h` -- 2D cell grid with cursor and scroll regions
- `sources/VT100Screen/VT100Screen.h` -- Complete screen state (grids + scrollback)
- `sources/VT100Screen/VT100ScreenMutableState.h` -- Mutation-thread state
- `sources/VT100Screen/VT100ScreenDelegate.h` -- Screen-to-session interface (~100 methods)
- `sources/VT100/VT100TerminalDelegate.h` -- Terminal-to-screen interface (~80 methods)
- `sources/ScreenChar/ScreenChar.h` -- Cell type definition
- `sources/LineBuffer/LineBuffer.h` -- Scrollback buffer (variable-width line storage)
- `sources/TerminalView/PTYTab.h` -- Tab/pane management
- `sources/TerminalView/PTYSplitView.h` -- Split view (thin NSSplitView subclass)
- `sources/TerminalView/PTYTextView.h` -- Text rendering view
- `sources/MetalRenderer/iTermMetalDriver.h` -- GPU rendering driver
- `sources/SessionCreation/iTermSessionFactory.h` -- Session creation/launch
- `sources/iTermController/iTermController.h` -- Application controller singleton
- `sources/Drawing/iTermTextDrawingHelper.h` -- Legacy CoreText rendering
