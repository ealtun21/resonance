# Resonance GUI Overhaul Mockup Polish — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Polish the "Resonance GUI overhaul" mockup (a Nocturne-design-system HTML/JS prototype) before it's ported into egui — fill the Effects/Mixer/Presets pages' empty space with real content, and make the whole mockup re-skin live across 7 concrete theme palettes seeded from the real app's `theme.rs`.

**Architecture:** The mockup is a single self-contained file, `Resonance Overhaul.dc.html`, built on a small custom "dc" runtime (`support.js`, generated, do-not-edit) that parses a `<x-dc>` template block (`{{ expr }}` bindings, `sc-if`/`sc-for` control-flow tags) plus a `<script data-dc-script>` block holding a `class Component extends DCLogic` with React-style `state`/`setState`. All visual tokens come from CSS custom properties defined in `_ds/nocturne-.../styles.css`. Theming is precomputed (not live OKLCH math) — 7 `:root[data-theme="..."]` override blocks were already generated via a throwaway Python/OKLCH script and are pasted verbatim in Task 2.

**Tech Stack:** Plain HTML/CSS/JS (Babel-in-browser via the dc runtime, React 18 UMD, Phosphor icons), no build step. Headless Chromium (`chromium --headless --screenshot`) for visual verification — there is no test framework because this is a static mockup, not application code.

## Global Constraints

- **Working copy lives outside git**, at `/home/nyverino/resonance-mockups/resonance-overhaul/` — matching where prior GUI mockups for this project have lived (`/home/nyverino/resonance-mockups/resonance-flat.html`, etc.). **Do not** commit these files to the `resonance` git repo. The only git-repo artifact for this work is the design doc already committed at `docs/superpowers/specs/2026-07-26-gui-overhaul-mockup-polish-design.md`.
- **No `git commit` steps in this plan.** Each task ends with a headless-Chromium screenshot review instead of a test run and a commit.
- **No changes to `resonance-gui`, `theme.rs`, or any other Rust code.** Real `theme.rs` values are read (not modified) as seed data for the precomputed theme blocks.
- **No changes to `support.js`** (generated runtime, has its own "do not edit" header) or to `Current UI (Recreation).dc.html`.
- Source zip for the initial copy: `/home/nyverino/Documents/resonance/Resonance GUI overhaul.zip` (already present in the repo's working directory, untracked — leave it there, don't delete or add it to git).
- Screenshots for verification: `chromium --headless --disable-gpu --no-sandbox --hide-scrollbars --window-size=1400,900 --screenshot="<out>.png" --virtual-time-budget=8000 --run-all-compositor-stages-before-draw "file://<path>"`. This mockup needs network access (unpkg.com for React/Babel/Phosphor) to render — confirm connectivity if a render comes back blank.
- To click into a specific page/theme/state before screenshotting, use Python Playwright driving system Chromium: `playwright.sync_api.sync_playwright().chromium.launch(executable_path='/usr/bin/chromium', args=['--no-sandbox'])`.

---

### Task 1: Stand up the working copy

**Files:**
- Create: `/home/nyverino/resonance-mockups/resonance-overhaul/` (directory, populated by unzip)

**Interfaces:**
- Produces: the working copy every later task edits in place. All later "Files" paths are relative to this directory.

- [ ] **Step 1: Create the destination directory and unzip the source into it**

```bash
mkdir -p /home/nyverino/resonance-mockups/resonance-overhaul
unzip -o "/home/nyverino/Documents/resonance/Resonance GUI overhaul.zip" -d /home/nyverino/resonance-mockups/resonance-overhaul
```

- [ ] **Step 2: Verify the expected files are present**

```bash
ls /home/nyverino/resonance-mockups/resonance-overhaul
```

Expected: `Current UI (Recreation).dc.html`, `Resonance Overhaul.dc.html`, `_ds/`, `docs/`, `support.js`, `.thumbnail`.

- [ ] **Step 3: Baseline-render to confirm the copy works before any edits**

```bash
cd /home/nyverino/resonance-mockups/resonance-overhaul
chromium --headless --disable-gpu --no-sandbox --hide-scrollbars \
  --window-size=1400,900 --screenshot="/tmp/baseline-overhaul.png" \
  --virtual-time-budget=8000 --run-all-compositor-stages-before-draw \
  "file://$(pwd)/Resonance Overhaul.dc.html"
```

Expected: `/tmp/baseline-overhaul.png` written, ~130KB+, showing the Equalize page in the default purple Nocturne theme (top bar "Resonance", left nav Equalize/Effects/Mixer/Presets/Setup, EQ graph). Open it (e.g. via the Read tool) and visually confirm before continuing.

---

### Task 2: Add boost/cut/highlight tokens and the 7 theme override blocks to `styles.css`

**Files:**
- Modify: `_ds/nocturne-cb4e457b-22e6-45ce-a4a8-0dd621dde14b/styles.css`

**Interfaces:**
- Produces: `--color-boost`, `--color-cut`, `--color-highlight` (new base tokens, default-theme values); 7 `:root[data-theme="..."]` blocks (`breeze-dark`, `gruvbox`, `nord`, `matrix`, `light`, `native-dark`, `native-light`), each overriding `--color-bg/-surface/-text/-accent/-accent-2/-boost/-cut/-highlight`, the `--color-neutral-100..900` / `--color-accent-100..900` / `--color-accent-2-100..900` ramps, and `--shadow-sm/-md/-lg`.
- Consumes: nothing (this is the token layer everything else reads from).

- [ ] **Step 1: Fix `--color-divider` to derive from the (now themeable) `--color-text` instead of a hardcoded hex**

In `:root { ... }`, find:

```css
  --color-divider: color-mix(in srgb, #e9e9ed 16%, transparent);
```

Replace with:

```css
  --color-divider: color-mix(in srgb, var(--color-text) 16%, transparent);
```

- [ ] **Step 2: Add the boost/cut/highlight base tokens**

Find:

```css
  --color-accent-2: #a7a1db; /* the machine-derived stand-in follows: same
     hue, same L 0.734, chroma at the same 2:3 proportion it held */
  --color-divider: color-mix(in srgb, var(--color-text) 16%, transparent);
```

Replace with (adds three lines after `--color-divider`):

```css
  --color-accent-2: #a7a1db; /* the machine-derived stand-in follows: same
     hue, same L 0.734, chroma at the same 2:3 proportion it held */
  --color-divider: color-mix(in srgb, var(--color-text) 16%, transparent);
  --color-boost: #6fbf9a;
  --color-cut: #c98a82;
  --color-highlight: #d8a35c;
```

- [ ] **Step 3: Append the 7 theme override blocks right after the base `:root { ... }` rule**

Find the end of the base token block:

```css
  --shadow-lg: 0 0 0 1px #9397ab, 0 16px 40px rgba(0,0,0,0.65);
}

body {
```

Replace with (inserts all 7 blocks between the closing `}` and `body {`):

```css
  --shadow-lg: 0 0 0 1px #9397ab, 0 16px 40px rgba(0,0,0,0.65);
}

  /* breeze-dark */
  :root[data-theme="breeze-dark"] {
    --color-bg: #0c1b24;
    --color-surface: #1a2831;
    --color-text: #e6eaed;
    --color-accent: #269dd7;
    --color-accent-2: #5a9bc1;
    --color-boost: #27ae60;
    --color-cut: #da4453;
    --color-highlight: #f67400;
    --color-neutral-100: #eef7fd;
    --color-neutral-200: #dceaf3;
    --color-neutral-300: #c5d7e3;
    --color-neutral-400: #a7bac8;
    --color-neutral-500: #889ba9;
    --color-neutral-600: #6a7d8a;
    --color-neutral-700: #51606a;
    --color-neutral-800: #39444c;
    --color-neutral-900: #262c30;
    --color-accent-100: #ecf7ff;
    --color-accent-200: #d2ecfd;
    --color-accent-300: #a9dcfb;
    --color-accent-400: #61c3fa;
    --color-accent-500: #2ea3de;
    --color-accent-600: #0084bd;
    --color-accent-700: #006693;
    --color-accent-800: #004869;
    --color-accent-900: #0e2f40;
    --color-accent-2-100: #eff7fc;
    --color-accent-2-200: #daebf6;
    --color-accent-2-300: #b8d9ee;
    --color-accent-2-400: #84c0e4;
    --color-accent-2-500: #60a1c7;
    --color-accent-2-600: #4182a7;
    --color-accent-2-700: #306481;
    --color-accent-2-800: #21475d;
    --color-accent-2-900: #1a2e3a;
    --shadow-sm: 0 0 0 1px #39444c;
    --shadow-md: 0 0 0 1px #51606a, 0 6px 18px rgba(0,0,0,0.55);
    --shadow-lg: 0 0 0 1px #889ba9, 0 16px 40px rgba(0,0,0,0.65);
  }

  /* gruvbox */
  :root[data-theme="gruvbox"] {
    --color-bg: #1e190a;
    --color-surface: #2b2618;
    --color-text: #e6ebe9;
    --color-accent: #799b8e;
    --color-accent-2: #82988f;
    --color-boost: #98971a;
    --color-cut: #cc241d;
    --color-highlight: #fabd2f;
    --color-neutral-100: #f9f5ec;
    --color-neutral-200: #ede8da;
    --color-neutral-300: #dad4c2;
    --color-neutral-400: #beb7a3;
    --color-neutral-500: #9f9884;
    --color-neutral-600: #817a67;
    --color-neutral-700: #635d4e;
    --color-neutral-800: #464237;
    --color-neutral-900: #2d2b25;
    --color-accent-100: #f2f7f5;
    --color-accent-200: #e1eae6;
    --color-accent-300: #c7d9d2;
    --color-accent-400: #a0c0b3;
    --color-accent-500: #7fa194;
    --color-accent-600: #618376;
    --color-accent-700: #4a645b;
    --color-accent-800: #344740;
    --color-accent-900: #242e2a;
    --color-accent-2-100: #f3f6f5;
    --color-accent-2-200: #e3e9e7;
    --color-accent-2-300: #cbd7d2;
    --color-accent-2-400: #a8bdb4;
    --color-accent-2-500: #889e95;
    --color-accent-2-600: #6a8077;
    --color-accent-2-700: #51625c;
    --color-accent-2-800: #394641;
    --color-accent-2-900: #262d2a;
    --shadow-sm: 0 0 0 1px #464237;
    --shadow-md: 0 0 0 1px #635d4e, 0 6px 18px rgba(0,0,0,0.55);
    --shadow-lg: 0 0 0 1px #9f9884, 0 16px 40px rgba(0,0,0,0.65);
  }

  /* nord */
  :root[data-theme="nord"] {
    --color-bg: #131926;
    --color-surface: #202632;
    --color-text: #e6eaec;
    --color-accent: #659cab;
    --color-accent-2: #7699a3;
    --color-boost: #a3be8c;
    --color-cut: #bf616a;
    --color-highlight: #ebcb8b;
    --color-neutral-100: #f1f6fe;
    --color-neutral-200: #e1e8f5;
    --color-neutral-300: #ccd4e5;
    --color-neutral-400: #aeb7ca;
    --color-neutral-500: #8f98ab;
    --color-neutral-600: #717a8c;
    --color-neutral-700: #575e6c;
    --color-neutral-800: #3d434d;
    --color-neutral-900: #282b31;
    --color-accent-100: #f0f7f9;
    --color-accent-200: #dcebef;
    --color-accent-300: #bddae2;
    --color-accent-400: #8ec1cf;
    --color-accent-500: #6ba2b2;
    --color-accent-600: #4d8393;
    --color-accent-700: #396571;
    --color-accent-800: #284851;
    --color-accent-900: #1e2e33;
    --color-accent-2-100: #f2f6f8;
    --color-accent-2-200: #e0eaed;
    --color-accent-2-300: #c5d8dd;
    --color-accent-2-400: #9dbec7;
    --color-accent-2-500: #7c9fa9;
    --color-accent-2-600: #5e818a;
    --color-accent-2-700: #47636b;
    --color-accent-2-800: #32464c;
    --color-accent-2-900: #232d30;
    --shadow-sm: 0 0 0 1px #3d434d;
    --shadow-md: 0 0 0 1px #575e6c, 0 6px 18px rgba(0,0,0,0.55);
    --shadow-lg: 0 0 0 1px #8f98ab, 0 16px 40px rgba(0,0,0,0.65);
  }

  /* matrix */
  :root[data-theme="matrix"] {
    --color-bg: #121d11;
    --color-surface: #1f291e;
    --color-text: #e7eae7;
    --color-accent: #00b800;
    --color-accent-2: #33ae41;
    --color-boost: #00e650;
    --color-cut: #007828;
    --color-highlight: #b4ff78;
    --color-neutral-100: #f1f8f0;
    --color-neutral-200: #e1ebe0;
    --color-neutral-300: #cbd8c9;
    --color-neutral-400: #adbcab;
    --color-neutral-500: #8e9d8c;
    --color-neutral-600: #707f6f;
    --color-neutral-700: #566155;
    --color-neutral-800: #3d453c;
    --color-neutral-900: #282d28;
    --color-accent-100: #e9fce8;
    --color-accent-200: #caf6ca;
    --color-accent-300: #96ed98;
    --color-accent-400: #00de35;
    --color-accent-500: #00bf00;
    --color-accent-600: #009e00;
    --color-accent-700: #007a00;
    --color-accent-800: #005700;
    --color-accent-900: #003700;
    --color-accent-2-100: #edfaed;
    --color-accent-2-200: #d4f1d4;
    --color-accent-2-300: #ade5ad;
    --color-accent-2-400: #6ad36f;
    --color-accent-2-500: #3ab447;
    --color-accent-2-600: #009424;
    --color-accent-2-700: #007219;
    --color-accent-2-800: #035211;
    --color-accent-2-900: #113413;
    --shadow-sm: 0 0 0 1px #3d453c;
    --shadow-md: 0 0 0 1px #566155, 0 6px 18px rgba(0,0,0,0.55);
    --shadow-lg: 0 0 0 1px #8e9d8c, 0 16px 40px rgba(0,0,0,0.65);
  }

  /* light */
  :root[data-theme="light"] {
    --color-bg: #f2f5fc;
    --color-surface: #fcfdff;
    --color-text: #1d222b;
    --color-accent: #3c97e9;
    --color-accent-2: #6097cd;
    --color-boost: #1e963c;
    --color-cut: #c83232;
    --color-highlight: #dc8200;
    --color-neutral-100: #f1f6fe;
    --color-neutral-200: #e1e8f5;
    --color-neutral-300: #ccd4e5;
    --color-neutral-400: #aeb7ca;
    --color-neutral-500: #8f98ab;
    --color-neutral-600: #717a8c;
    --color-neutral-700: #575e6c;
    --color-neutral-800: #3d434d;
    --color-neutral-900: #282b31;
    --color-accent-100: #edf7ff;
    --color-accent-200: #d4ebff;
    --color-accent-300: #add9ff;
    --color-accent-400: #6dbdff;
    --color-accent-500: #439df0;
    --color-accent-600: #1c7dce;
    --color-accent-700: #1360a0;
    --color-accent-800: #0d4473;
    --color-accent-900: #132d46;
    --color-accent-2-100: #f0f6fd;
    --color-accent-2-200: #dbeafa;
    --color-accent-2-300: #bbd7f5;
    --color-accent-2-400: #89bcf0;
    --color-accent-2-500: #669dd4;
    --color-accent-2-600: #487eb3;
    --color-accent-2-700: #36618b;
    --color-accent-2-800: #264563;
    --color-accent-2-900: #1c2d3d;
    --shadow-sm: 0 0 0 1px #3d434d;
    --shadow-md: 0 0 0 1px #575e6c, 0 6px 18px rgba(0,0,0,0.55);
    --shadow-lg: 0 0 0 1px #8f98ab, 0 16px 40px rgba(0,0,0,0.65);
  }

  /* native-dark */
  :root[data-theme="native-dark"] {
    --color-bg: #141926;
    --color-surface: #222532;
    --color-text: #e7eaed;
    --color-accent: #348dff;
    --color-accent-2: #5b92e5;
    --color-boost: #50c878;
    --color-cut: #e65a5f;
    --color-highlight: #f67400;
    --color-neutral-100: #f2f5fe;
    --color-neutral-200: #e3e7f5;
    --color-neutral-300: #ced3e5;
    --color-neutral-400: #b0b7ca;
    --color-neutral-500: #9198ab;
    --color-neutral-600: #737a8c;
    --color-neutral-700: #585d6c;
    --color-neutral-800: #3f424d;
    --color-neutral-900: #292b31;
    --color-accent-100: #ecf6ff;
    --color-accent-200: #d1eaff;
    --color-accent-300: #a8d6ff;
    --color-accent-400: #65b6ff;
    --color-accent-500: #3b94ff;
    --color-accent-600: #1373f1;
    --color-accent-700: #0b58bc;
    --color-accent-800: #083f87;
    --color-accent-900: #102a51;
    --color-accent-2-100: #eff6ff;
    --color-accent-2-200: #d9e9ff;
    --color-accent-2-300: #b8d6ff;
    --color-accent-2-400: #84b9ff;
    --color-accent-2-500: #6198ec;
    --color-accent-2-600: #4379ca;
    --color-accent-2-700: #325d9d;
    --color-accent-2-800: #234271;
    --color-accent-2-900: #1b2b45;
    --shadow-sm: 0 0 0 1px #3f424d;
    --shadow-md: 0 0 0 1px #585d6c, 0 6px 18px rgba(0,0,0,0.55);
    --shadow-lg: 0 0 0 1px #9198ab, 0 16px 40px rgba(0,0,0,0.65);
  }

  /* native-light */
  :root[data-theme="native-light"] {
    --color-bg: #f2f5fc;
    --color-surface: #fcfdff;
    --color-text: #1d222b;
    --color-accent: #348dff;
    --color-accent-2: #5b92e5;
    --color-boost: #28a050;
    --color-cut: #c83c3c;
    --color-highlight: #f67400;
    --color-neutral-100: #f1f6fe;
    --color-neutral-200: #e1e8f5;
    --color-neutral-300: #ccd4e5;
    --color-neutral-400: #aeb7ca;
    --color-neutral-500: #8f98ab;
    --color-neutral-600: #717a8c;
    --color-neutral-700: #575e6c;
    --color-neutral-800: #3d434d;
    --color-neutral-900: #282b31;
    --color-accent-100: #ecf6ff;
    --color-accent-200: #d1eaff;
    --color-accent-300: #a8d6ff;
    --color-accent-400: #65b6ff;
    --color-accent-500: #3b94ff;
    --color-accent-600: #1373f1;
    --color-accent-700: #0b58bc;
    --color-accent-800: #083f87;
    --color-accent-900: #102a51;
    --color-accent-2-100: #eff6ff;
    --color-accent-2-200: #d9e9ff;
    --color-accent-2-300: #b8d6ff;
    --color-accent-2-400: #84b9ff;
    --color-accent-2-500: #6198ec;
    --color-accent-2-600: #4379ca;
    --color-accent-2-700: #325d9d;
    --color-accent-2-800: #234271;
    --color-accent-2-900: #1b2b45;
    --shadow-sm: 0 0 0 1px #3d434d;
    --shadow-md: 0 0 0 1px #575e6c, 0 6px 18px rgba(0,0,0,0.55);
    --shadow-lg: 0 0 0 1px #8f98ab, 0 16px 40px rgba(0,0,0,0.65);
  }

body {
```

- [ ] **Step 4: Verify with a quick DOM check (no visual dependency)**

```bash
cd /home/nyverino/resonance-mockups/resonance-overhaul
grep -c 'data-theme=' "_ds/nocturne-cb4e457b-22e6-45ce-a4a8-0dd621dde14b/styles.css"
```

Expected: `7`.

*(Where these 7 blocks' values came from: each seeds `accent`/`boost`/`cut`/`highlight`/`graph_bg` from the real `Theme::palette()` match arms in `crates/resonance-gui/src/theme.rs` — Breeze Dark/Gruvbox/Nord/Matrix/Light read verbatim; `native-dark`/`native-light` use `native_palette_for`'s hardcoded boost/cut/graph_bg with a representative accent `#0a6eeb`/`#348dff`-family blue standing in for the OS-read value, since "Native" has no fixed color to seed from. Ramps were generated by converting the current default purple ramp to OKLCH, reusing its exact lightness steps and its accent-ramp chroma-to-base-chroma ratio, re-hued/re-chromaed per theme — do not hand-tune these further; if a value ever needs changing, regenerate from the seed table rather than hand-editing one hex.)*

---

### Task 3: Wire the Setup → Appearance buttons to a real theme switch

**Files:**
- Modify: `Resonance Overhaul.dc.html`

**Interfaces:**
- Consumes: the 7 `[data-theme]` blocks from Task 2.
- Produces: `state.theme` (string, one of `native-auto`/`native-dark`/`native-light`/`breeze-dark`/`gruvbox`/`nord`/`matrix`/`light`/`matugen-auto`), and a live `data-theme` attribute on `<html>` that later tasks' verification screenshots rely on to preview themes.

- [ ] **Step 1: Add `theme` to initial state**

Find:

```js
  state = { mode: this.props.startMode ?? 'advanced', page: 'eq', sel: 0, ref: true, raw: false, bounds: true, power: true, tour: null,
    simpleTab: 'auto', gear: 'headphones', lt: 'idle', ltTrial: 1,
    simpleGains: [2,1,0,0,-1,1,2,1] };
```

Replace with:

```js
  state = { mode: this.props.startMode ?? 'advanced', page: 'eq', sel: 0, ref: true, raw: false, bounds: true, power: true, tour: null,
    simpleTab: 'auto', gear: 'headphones', lt: 'idle', ltTrial: 1,
    simpleGains: [2,1,0,0,-1,1,2,1], theme: 'native-auto', fxExpanded: null, chVisible: [true,true] };
```

(`fxExpanded` and `chVisible` are added here too — they're used by Tasks 5 and 6, declaring them together keeps the state shape in one place.)

- [ ] **Step 2: Rewrite `themes()` to be state-driven with working click handlers**

Find:

```js
  themes() {
    const list = [['Native (auto)','#9184d9',true],['Native Dark','#9184d9',false],['Native Light','#e4e6f0',false],['Breeze Dark','#3daee9',false],['Gruvbox','#83a598',false],['Nord','#88c0d0',false],['Matrix','#00ff46',false],['Light','#1478c8',false],['Matugen (auto)','#c9a15c',false]];
    return list.map(([name,sw,on])=>({
      name,
      swatchStyle: {width:'10px',height:'10px',borderRadius:'50%',background:sw,flex:'none',opacity:0.9},
      style: {display:'flex',alignItems:'center',gap:'7px',height:'28px',padding:'0 11px',borderRadius:'14px',whiteSpace:'nowrap',cursor:'pointer',fontSize:'12px',
        border: on ? '1px solid var(--color-accent)' : '1px solid var(--color-neutral-800)',
        background: on ? 'rgba(145,132,217,0.12)' : 'transparent',
        color: on ? '#b3a9e6' : 'var(--color-neutral-300)'}
    }));
  }
```

Replace with:

```js
  themes() {
    const list = [
      ['Native (auto)','native-auto','#9397ab'],
      ['Native Dark','native-dark','#0a6eeb'],
      ['Native Light','native-light','#0a6eeb'],
      ['Breeze Dark','breeze-dark','#3daee9'],
      ['Gruvbox','gruvbox','#83a598'],
      ['Nord','nord','#88c0d0'],
      ['Matrix','matrix','#00ff46'],
      ['Light','light','#1478c8'],
      ['Matugen (auto)','matugen-auto','#c9a15c']
    ];
    const current = this.state.theme;
    return list.map(([name,id,sw])=>{
      const on = current===id;
      return {
        name,
        swatchStyle: {width:'10px',height:'10px',borderRadius:'50%',background:sw,flex:'none',opacity:0.9},
        style: {display:'flex',alignItems:'center',gap:'7px',height:'28px',padding:'0 11px',borderRadius:'14px',whiteSpace:'nowrap',cursor:'pointer',fontSize:'12px',fontFamily:'inherit',
          border: on ? '1px solid var(--color-accent)' : '1px solid var(--color-neutral-800)',
          background: on ? 'color-mix(in srgb, var(--color-accent) 12%, transparent)' : 'transparent',
          color: on ? 'var(--color-accent-200)' : 'var(--color-neutral-300)'},
        go: () => {
          if (id === 'native-auto' || id === 'matugen-auto') {
            document.documentElement.removeAttribute('data-theme');
          } else {
            document.documentElement.setAttribute('data-theme', id);
          }
          this.setState({theme:id});
        }
      };
    });
  }
```

- [ ] **Step 3: Make the Appearance buttons clickable in the template**

In the `<!-- ── SETTINGS page ── -->` section, find:

```html
              <sc-for list="{{ themes }}" as="th" hint-placeholder-count="9">
                <span style="{{ th.style }}"><span style="{{ th.swatchStyle }}"></span>{{ th.name }}</span>
              </sc-for>
```

Replace with:

```html
              <sc-for list="{{ themes }}" as="th" hint-placeholder-count="9">
                <button onClick="{{ th.go }}" style="{{ th.style }}" style-hover="border-color: var(--color-accent-700)"><span style="{{ th.swatchStyle }}"></span>{{ th.name }}</button>
              </sc-for>
```

- [ ] **Step 4: Visual check — click through the theme buttons**

```bash
cd /home/nyverino/resonance-mockups/resonance-overhaul
python3 -c "
from playwright.sync_api import sync_playwright
import os
with sync_playwright() as p:
    browser = p.chromium.launch(executable_path='/usr/bin/chromium', args=['--no-sandbox'])
    page = browser.new_page(viewport={'width':1400,'height':900})
    page.goto('file://' + os.path.abspath('Resonance Overhaul.dc.html'), wait_until='networkidle', timeout=30000)
    page.wait_for_timeout(1200)
    page.get_by_text('Setup', exact=True).first.click()
    page.wait_for_timeout(400)
    page.get_by_text('Nord', exact=True).first.click()
    page.wait_for_timeout(400)
    print('data-theme =', page.eval_on_selector('html', 'el => el.getAttribute(\"data-theme\")'))
    page.screenshot(path='/tmp/task3-nord-setup.png')
    browser.close()
"
```

Expected: prints `data-theme = nord`; `/tmp/task3-nord-setup.png` shows the Setup page's cards, borders, and the Nord button's own highlighted state re-tinted toward Nord's blue-teal rather than the original purple (background/card colors will look right; the *curve* and most tinted-hover colors won't fully match yet — that's expected, Task 4 fixes those). Open the screenshot and confirm the Nord button itself is now selected/highlighted and the page background/cards shifted tone.

---

### Task 4: Make every hardcoded accent/success/danger/grid color themeable

This is a mechanical global-replacement pass across `Resonance Overhaul.dc.html` (both the HTML template and the JS). Every replacement below is a plain string substitution — do them as `replace_all` for a given file unless a step says otherwise. Do this task **after** Task 3, since Task 3 already rewrote `themes()`'s swatch-color literals (which must stay literal, not be swept up in this pass) — by the time this task runs, none of the strings below appear inside `themes()` anymore.

**Files:**
- Modify: `Resonance Overhaul.dc.html`

**Interfaces:**
- Consumes: `--color-accent-200/700/900`, `--color-boost`, `--color-cut`, `--color-highlight`, `--color-neutral-400/500/700/800/900`, `--color-bg`, `--color-text` (all from Task 2).
- Produces: an EQ curve, grid, legend, selected-state, and status-indicator set of colors that all re-theme when `data-theme` changes — what Task 3 wired up now visibly works end-to-end.

- [ ] **Step 1: Replace bare hex literals, file-wide, in this exact order**

| Find (exact substring) | Replace with |
|---|---|
| `stroke:'#9184d9'` | `stroke:'var(--color-accent)'` |
| `background:isSel?'#9184d9':'#161826'` | `background:isSel?'var(--color-accent)':'var(--color-bg)'` |
| `border:isSel?'2px solid #e9e9ed':'1.4px solid #9184d9'` | `border:isSel?'2px solid var(--color-text)':'1.4px solid var(--color-accent)'` |
| `['EQ','#9184d9','solid']` | `['EQ','var(--color-accent)','solid']` |
| `stroke:'#d8a35c'` | `stroke:'var(--color-highlight)'` |
| `['Result','#d8a35c','solid']` | `['Result','var(--color-highlight)','solid']` |
| `stroke:'#2b2f45'` | `stroke:'var(--color-neutral-800)'` |
| `stroke:'#232741'` | `stroke:'var(--color-neutral-900)'` |
| `stroke:'#20233a'` | `stroke:'var(--color-neutral-900)'` |
| `stroke:'#3d4160'` | `stroke:'var(--color-neutral-700)'` |
| `fill:'#5c6180'` | `fill:'var(--color-neutral-500)'` |
| `fill:'#8b8fa3'` | `fill:'var(--color-neutral-400)'` |
| `i===sel ? '#e9e9ed' : 'var(--color-neutral-300)'` | `i===sel ? 'var(--color-text)' : 'var(--color-neutral-300)'` |
| `active ? '#e9e9ed' : 'var(--color-neutral-200)'` | `active ? 'var(--color-text)' : 'var(--color-neutral-200)'` |

Note: `stroke:'#9184d9'` is intentionally applied as one `replace_all` even though it matches 4 separate call sites (the EQ curve in `graph()`, `miniCurve()`, and `simpleCurve()`, plus `simpleHero()`) — all 4 are the same "this is the accent-colored curve line" role, so one replacement is correct for all of them. The `#161826`/`#e9e9ed` bare hexes are handled only via the two dedicated full-context rows above (and the two `i===sel`/`active` rows below) — do **not** additionally blind-`replace_all` bare `#161826` or `#e9e9ed`, since both hexes also appear as the fallback value inside `var(--color-bg,#161826)` / `var(--color-text,#e9e9ed)` in the top `<style>` block, which must stay as literal fallbacks, not become `var(--color-bg,var(--color-bg))`.

- [ ] **Step 2: Replace the remaining `#b3a9e6` occurrences (accent-200 stand-in)**

These appear embedded in larger inline object/string literals. Replace_all `#b3a9e6` → `var(--color-accent-200)` across the whole file. This touches (at minimum) the `tab()`/`gearBtn()` "on" text colors in `simpleVals()`, the reference-target dashed-line stroke in `graph()`, the legend `['Target','#b3a9e6','dash']` entry, and the `seg()`/`pill()`/`navBtn()`-adjacent "on" colors in `renderVals()`.

- [ ] **Step 3: Replace the boost/cut bare hex literals**

Replace_all, file-wide:
- `#6fbf9a` → `var(--color-boost)`
- `#c98a82` → `var(--color-cut)`

This covers: the two "Tuned for..."/"ear profile ready" checkmark icons, the "matched" tag, the "no clipping" tag, the daemon-running status dot, the status-bar "OK", `valCol`/`gainCol`'s boost branch, `muteCol`'s cut branch, the active-output `tagCol`, and half of `powerStyle`/`powerDotStyle`.

- [ ] **Step 4: Replace the accent-tint `rgba(145,132,217,X)` family with `color-mix()`**

Replace_all, file-wide, one row at a time (order doesn't matter, they're disjoint strings):

| Find | Replace with |
|---|---|
| `rgba(145,132,217,0.07)` | `color-mix(in srgb, var(--color-accent) 7%, transparent)` |
| `rgba(145,132,217,0.08)` | `color-mix(in srgb, var(--color-accent) 8%, transparent)` |
| `rgba(145,132,217,0.09)` | `color-mix(in srgb, var(--color-accent) 9%, transparent)` |
| `rgba(145,132,217,0.10)` | `color-mix(in srgb, var(--color-accent) 10%, transparent)` |
| `rgba(145,132,217,0.12)` | `color-mix(in srgb, var(--color-accent) 12%, transparent)` |
| `rgba(145,132,217,0.14)` | `color-mix(in srgb, var(--color-accent) 14%, transparent)` |
| `rgba(145,132,217,0.16)` | `color-mix(in srgb, var(--color-accent) 16%, transparent)` |
| `rgba(145,132,217,0.2)` | `color-mix(in srgb, var(--color-accent) 20%, transparent)` |
| `rgba(145,132,217,0.22)` | `color-mix(in srgb, var(--color-accent) 22%, transparent)` |
| `rgba(145,132,217,0.30)` | `color-mix(in srgb, var(--color-accent) 30%, transparent)` |
| `rgba(145,132,217,0.35)` | `color-mix(in srgb, var(--color-accent) 35%, transparent)` |
| `rgba(145,132,217,0.5)` | `color-mix(in srgb, var(--color-accent) 50%, transparent)` |

- [ ] **Step 5: Replace the boost/cut rgba tint families**

| Find | Replace with |
|---|---|
| `rgba(111,191,154,0.08)` | `color-mix(in srgb, var(--color-boost) 8%, transparent)` |
| `rgba(111,191,154,0.4)` | `color-mix(in srgb, var(--color-boost) 40%, transparent)` |
| `rgba(111,191,154,0.5)` | `color-mix(in srgb, var(--color-boost) 50%, transparent)` |
| `rgba(201,138,130,0.08)` | `color-mix(in srgb, var(--color-cut) 8%, transparent)` |
| `rgba(201,138,130,0.5)` | `color-mix(in srgb, var(--color-cut) 50%, transparent)` |

- [ ] **Step 6: Confirm no unthemed literals remain**

```bash
cd /home/nyverino/resonance-mockups/resonance-overhaul
grep -c "#9184d9\|#b3a9e6\|#6fbf9a\|#c98a82\|#d8a35c\|#2b2f45\|#232741\|#20233a\|#3d4160\|#5c6180\|#8b8fa3\|145,132,217\|111,191,154\|201,138,130" "Resonance Overhaul.dc.html"
```

Expected: `0`. The 9 deliberate swatch-dot hexes inside the new `themes()` list from Task 3 (`#3daee9`, `#83a598`, `#88c0d0`, `#00ff46`, `#1478c8`, `#9397ab`, `#0a6eeb` ×2, `#c9a15c`) are distinct hex values that don't match any pattern in this grep, so they don't affect the count. If the count isn't `0`, `grep -n` the same pattern to see what's left and fix it before moving on.

- [ ] **Step 7: Visual check — the curve itself must re-theme**

```bash
cd /home/nyverino/resonance-mockups/resonance-overhaul
python3 -c "
from playwright.sync_api import sync_playwright
import os
with sync_playwright() as p:
    browser = p.chromium.launch(executable_path='/usr/bin/chromium', args=['--no-sandbox'])
    page = browser.new_page(viewport={'width':1400,'height':900})
    page.goto('file://' + os.path.abspath('Resonance Overhaul.dc.html'), wait_until='networkidle', timeout=30000)
    page.wait_for_timeout(1200)
    page.get_by_text('Setup', exact=True).first.click()
    page.wait_for_timeout(300)
    page.get_by_text('Matrix', exact=True).first.click()
    page.wait_for_timeout(300)
    page.get_by_text('Equalize', exact=True).first.click()
    page.wait_for_timeout(500)
    page.screenshot(path='/tmp/task4-matrix-eq.png')
    browser.close()
"
```

Expected: `/tmp/task4-matrix-eq.png` shows the EQ curve, band nodes, and reference legend in Matrix's green, not the original purple. Open and visually confirm.

---

### Task 5: Effects page — expandable per-effect detail, no forced fill

**Files:**
- Modify: `Resonance Overhaul.dc.html`

**Interfaces:**
- Consumes: `state.fxExpanded` (added in Task 3 Step 1), `var(--color-accent-900)` and other tokens from Tasks 2/4.
- Produces: `fxCards[].expanded/toggleExpand/caretRot/detail`, `convExpanded/convToggleExpand/convCaretRot/convDetail`, a new `effectDetail(kind)` / `effectCurveSvg(shapeFn)` method pair other tasks don't need.

Note on "no forced fill": inspecting the actual markup, the Effects/Mixer/Presets page containers were never `flex:1`-stretched to fill the viewport — they're plain content-sized flex children inside a taller `flex:1` page-area container, so the empty space below short content is just that container's own background showing through, not a bug to remove. There is no CSS to delete here; the fix is purely additive (the accordions below), which is what actually resolves the user-visible complaint.

- [ ] **Step 1: Add the detail-content generator methods**

Find (this is right before `themes()`, i.e. right after `bands()`'s closing brace and before `graph(sel, refOn, ...)` — anchor on the `bands()` method to place it precisely):

```js
  bands() {
    return [
      {t:'LS',f:41,g:4.7,q:0.55,s:'12 dB/oct'},{t:'HS',f:14216,g:-3.1,q:1.33,s:'12 dB/oct'},
      {t:'PK',f:285,g:-7.1,q:0.40,s:'—'},{t:'PK',f:81,g:1.5,q:1.69,s:'—'},
      {t:'PK',f:2408,g:-2.8,q:2.63,s:'—'},{t:'PK',f:4129,g:9.9,q:3.92,s:'—'},
      {t:'PK',f:7889,g:-12.6,q:0.73,s:'—'},{t:'PK',f:8600,g:11.6,q:2.25,s:'—'},
      {t:'PK',f:52,g:1.0,q:0.96,s:'—'},{t:'PK',f:1373,g:-3.5,q:0.87,s:'—'}
    ];
  }
```

Replace with (adds two new methods after it, `bands()` itself unchanged):

```js
  bands() {
    return [
      {t:'LS',f:41,g:4.7,q:0.55,s:'12 dB/oct'},{t:'HS',f:14216,g:-3.1,q:1.33,s:'12 dB/oct'},
      {t:'PK',f:285,g:-7.1,q:0.40,s:'—'},{t:'PK',f:81,g:1.5,q:1.69,s:'—'},
      {t:'PK',f:2408,g:-2.8,q:2.63,s:'—'},{t:'PK',f:4129,g:9.9,q:3.92,s:'—'},
      {t:'PK',f:7889,g:-12.6,q:0.73,s:'—'},{t:'PK',f:8600,g:11.6,q:2.25,s:'—'},
      {t:'PK',f:52,g:1.0,q:0.96,s:'—'},{t:'PK',f:1373,g:-3.5,q:0.87,s:'—'}
    ];
  }

  effectCurveSvg(shapeFn) {
    const W = 560, H = 64;
    const X = i => W * i;
    const Y = v => H/2 - v * (H/2 - 6);
    let d = '';
    for (let i=0;i<=120;i++){ const t = i/120; const v = Math.max(-1,Math.min(1,shapeFn(t))); d += (i?'L':'M')+X(t).toFixed(1)+' '+Y(v).toFixed(1); }
    const e = React.createElement;
    return e('svg',{width:'100%',height:H,viewBox:`0 0 ${W} ${H}`,preserveAspectRatio:'none',style:{display:'block'}},
      e('line',{x1:0,y1:Y(0),x2:W,y2:Y(0),stroke:'var(--color-neutral-800)',strokeWidth:1}),
      e('path',{d:d+`L${W} ${H}L0 ${H}Z`,fill:'var(--color-accent-900)'}),
      e('path',{d,stroke:'var(--color-accent)',strokeWidth:1.8,fill:'none'}));
  }

  effectDetail(kind) {
    const e = React.createElement;
    if (kind === 'width') {
      return e('div',{style:{display:'flex',flexDirection:'column',gap:'8px'}},
        e('div',{style:{fontSize:'11.5px',color:'var(--color-neutral-400)'}},'Stereo field width'),
        e('div',{style:{height:'8px',borderRadius:'4px',background:'var(--color-neutral-800)',position:'relative'}},
          e('div',{style:{position:'absolute',left:'15%',right:'15%',top:0,bottom:0,borderRadius:'4px',background:'var(--color-accent)'}})),
        e('div',{style:{display:'flex',justifyContent:'space-between',fontSize:'10.5px',color:'var(--color-neutral-500)',fontFamily:'ui-monospace,monospace'}},
          e('span',null,'L'),e('span',null,'mono → 235% wide'),e('span',null,'R')));
    }
    if (kind === 'levels') {
      const rows = [['Before peak','−0.2 dB','92%'],['After peak','−3.1 dB','68%'],['Makeup gain','+6.4 dB','100%']];
      return e('div',{style:{display:'flex',flexDirection:'column',gap:'6px'}},
        rows.map(([label,val,w],i)=>e('div',{key:i,style:{display:'flex',alignItems:'center',gap:'8px'}},
          e('span',{style:{fontSize:'11px',color:'var(--color-neutral-400)',width:'80px',flex:'none'}},label),
          e('div',{style:{flex:1,height:'6px',borderRadius:'3px',background:'var(--color-neutral-800)',position:'relative'}},
            e('div',{style:{position:'absolute',left:0,top:0,bottom:0,width:w,borderRadius:'3px',background:'var(--color-accent)'}})),
          e('span',{style:{fontSize:'11px',color:'var(--color-neutral-300)',fontFamily:'ui-monospace,monospace',width:'56px',textAlign:'right'}},val))));
    }
    if (kind === 'mix') {
      return e('div',{style:{display:'flex',flexDirection:'column',gap:'8px'}},
        e('div',{style:{fontSize:'11.5px',color:'var(--color-neutral-400)'}},'700 Hz low-pass crossfeed to the opposite ear'),
        e('div',{style:{display:'flex',alignItems:'center',gap:'10px'}},
          e('span',{style:{width:'26px',height:'26px',borderRadius:'50%',background:'var(--color-accent)',color:'var(--color-bg)',display:'flex',alignItems:'center',justifyContent:'center',fontSize:'11px',fontFamily:'ui-monospace,monospace'}},'L'),
          e('div',{style:{flex:1,height:'2px',background:'var(--color-neutral-700)'}}),
          e('span',{style:{fontSize:'10.5px',color:'var(--color-neutral-500)',whiteSpace:'nowrap'}},'30% mix'),
          e('div',{style:{flex:1,height:'2px',background:'var(--color-neutral-700)'}}),
          e('span',{style:{width:'26px',height:'26px',borderRadius:'50%',background:'var(--color-accent)',color:'var(--color-bg)',display:'flex',alignItems:'center',justifyContent:'center',fontSize:'11px',fontFamily:'ui-monospace,monospace'}},'R')));
    }
    const shapes = {
      'curve-fidelity': i => i<0.6 ? 0 : (i-0.6)/0.4*0.85,
      'curve-ambience': i => 0.3*Math.sin(i*Math.PI*3)*Math.exp(-i*1.5),
      'curve-bass': i => i<0.25 ? 0.7*(1-i/0.25) : 0,
      'curve-loudness': i => 0.55*Math.pow(2*i-1,2) - 0.18,
      'ir': t => Math.sin(t*40)*Math.exp(-t*4)
    };
    return this.effectCurveSvg(shapes[kind] || shapes['curve-fidelity']);
  }
```

- [ ] **Step 2: Give each `fxData` row a detail `kind`, and derive `expanded`/`toggleExpand`/`caretRot`/`detail` on the mapped cards**

Find:

```js
    const fxData = [
      ['Fidelity','ph ph-diamond',true,72,'Restores the sparkle lost to lossy compression — adds harmonics above the mix.'],
      ['Ambience','ph ph-cube-transparent',true,35,'Widens the room around the sound; subtle early reflections.'],
      ['Surround','ph ph-arrows-out',true,50,'Stereo field width, from mono-safe to super-wide.'],
      ['Dynamic Boost','ph ph-lightning',true,40,'Loudness-aware punch — lifts quiet passages without crushing peaks.'],
      ['Bass','ph ph-speaker-low',true,55,'Harmonic bass extension your drivers can actually reproduce.'],
      ['Loudness','ph ph-ear',false,0,'ISO 226:2023 equal-loudness compensation at low listening volumes.'],
      ['Crossfeed','ph ph-headphones',false,30,'Feeds a touch of each channel to the other ear — speaker-like headphone imaging.']
    ];
    const fxCards = fxData.map(([name,icon,on,v,desc])=>({
      name, icon, desc, pct: (v>0?'+':'')+v+'%',
      nameCol: on ? 'var(--color-text)' : 'var(--color-neutral-500)',
      iconCol: on ? 'var(--color-accent)' : 'var(--color-neutral-600)',
      toggleStyle: on ? toggleOn : toggleOff,
      knobStyle: on ? knobOn : knobOff,
      fillCol: on ? 'var(--color-accent)' : 'var(--color-neutral-700)',
      fillLeft: '0%', fillW: v+'%', handleLeft: v+'%'
    }));
```

Replace with:

```js
    const fxData = [
      ['Fidelity','ph ph-diamond',true,72,'Restores the sparkle lost to lossy compression — adds harmonics above the mix.','curve-fidelity'],
      ['Ambience','ph ph-cube-transparent',true,35,'Widens the room around the sound; subtle early reflections.','curve-ambience'],
      ['Surround','ph ph-arrows-out',true,50,'Stereo field width, from mono-safe to super-wide.','width'],
      ['Dynamic Boost','ph ph-lightning',true,40,'Loudness-aware punch — lifts quiet passages without crushing peaks.','levels'],
      ['Bass','ph ph-speaker-low',true,55,'Harmonic bass extension your drivers can actually reproduce.','curve-bass'],
      ['Loudness','ph ph-ear',false,0,'ISO 226:2023 equal-loudness compensation at low listening volumes.','curve-loudness'],
      ['Crossfeed','ph ph-headphones',false,30,'Feeds a touch of each channel to the other ear — speaker-like headphone imaging.','mix']
    ];
    const fxCards = fxData.map(([name,icon,on,v,desc,kind])=>{
      const expanded = this.state.fxExpanded === name;
      return {
        name, icon, desc, pct: (v>0?'+':'')+v+'%',
        nameCol: on ? 'var(--color-text)' : 'var(--color-neutral-500)',
        iconCol: on ? 'var(--color-accent)' : 'var(--color-neutral-600)',
        toggleStyle: on ? toggleOn : toggleOff,
        knobStyle: on ? knobOn : knobOff,
        fillCol: on ? 'var(--color-accent)' : 'var(--color-neutral-700)',
        fillLeft: '0%', fillW: v+'%', handleLeft: v+'%',
        expanded,
        toggleExpand: () => this.setState({fxExpanded: expanded ? null : name}),
        caretRot: expanded ? 'rotate(180deg)' : 'rotate(0deg)',
        detail: this.effectDetail(kind)
      };
    });
```

- [ ] **Step 3: Update the Effects-page template — clickable description row + detail panel, per card**

Find:

```html
        <sc-for list="{{ fxCards }}" as="fx" hint-placeholder-count="5">
          <div style="background:var(--color-neutral-900);border:1px solid var(--color-neutral-800);border-radius:10px;padding:14px 16px;display:flex;flex-direction:column;gap:10px">
            <div style="display:flex;align-items:center;gap:10px">
              <i class="{{ fx.icon }}" style="font-size:17px;color: {{ fx.iconCol }}"></i>
              <span style="font-size:13.5px;flex:1;color: {{ fx.nameCol }}">{{ fx.name }}</span>
              <span style="font-family:ui-monospace,monospace;font-size:12px;color:var(--color-neutral-400)">{{ fx.pct }}</span>
              <span style="{{ fx.toggleStyle }}"><span style="{{ fx.knobStyle }}"></span></span>
            </div>
            <div style="height:16px;position:relative">
              <div style="position:absolute;left:0;right:0;top:6px;height:4px;border-radius:2px;background:var(--color-neutral-800)"></div>
              <div style="position:absolute;top:6px;height:4px;border-radius:2px;background: {{ fx.fillCol }};left: {{ fx.fillLeft }};width: {{ fx.fillW }}"></div>
              <div style="position:absolute;top:1px;width:14px;height:14px;border-radius:50%;background: {{ fx.fillCol }};transform:translateX(-50%);left: {{ fx.handleLeft }}"></div>
            </div>
            <div style="font-size:11.5px;color:var(--color-neutral-500);line-height:1.45">{{ fx.desc }}</div>
          </div>
        </sc-for>
```

Replace with:

```html
        <sc-for list="{{ fxCards }}" as="fx" hint-placeholder-count="5">
          <div style="background:var(--color-neutral-900);border:1px solid var(--color-neutral-800);border-radius:10px;padding:14px 16px;display:flex;flex-direction:column;gap:10px">
            <div style="display:flex;align-items:center;gap:10px">
              <i class="{{ fx.icon }}" style="font-size:17px;color: {{ fx.iconCol }}"></i>
              <span style="font-size:13.5px;flex:1;color: {{ fx.nameCol }}">{{ fx.name }}</span>
              <span style="font-family:ui-monospace,monospace;font-size:12px;color:var(--color-neutral-400)">{{ fx.pct }}</span>
              <span style="{{ fx.toggleStyle }}"><span style="{{ fx.knobStyle }}"></span></span>
            </div>
            <div style="height:16px;position:relative">
              <div style="position:absolute;left:0;right:0;top:6px;height:4px;border-radius:2px;background:var(--color-neutral-800)"></div>
              <div style="position:absolute;top:6px;height:4px;border-radius:2px;background: {{ fx.fillCol }};left: {{ fx.fillLeft }};width: {{ fx.fillW }}"></div>
              <div style="position:absolute;top:1px;width:14px;height:14px;border-radius:50%;background: {{ fx.fillCol }};transform:translateX(-50%);left: {{ fx.handleLeft }}"></div>
            </div>
            <div onClick="{{ fx.toggleExpand }}" style="display:flex;align-items:center;gap:8px;cursor:pointer">
              <div style="flex:1;font-size:11.5px;color:var(--color-neutral-500);line-height:1.45">{{ fx.desc }}</div>
              <i class="ph ph-caret-down" style="font-size:11px;color:var(--color-neutral-500);flex:none;transform: {{ fx.caretRot }}"></i>
            </div>
            <sc-if value="{{ fx.expanded }}" hint-placeholder-val="{{ false }}">
              <div style="padding-top:6px;border-top:1px solid var(--color-neutral-800)">{{ fx.detail }}</div>
            </sc-if>
          </div>
        </sc-for>
```

- [ ] **Step 4: Give the Convolution card the same expand treatment (Output dither is left as a plain card — nothing to expand)**

Find:

```html
          <div style="background:var(--color-neutral-900);border:1px solid var(--color-neutral-800);border-radius:10px;padding:14px 16px;display:flex;flex-direction:column;gap:10px">
            <div style="display:flex;align-items:center;gap:10px">
              <i class="ph ph-wave-sine" style="font-size:17px;color:var(--color-neutral-300)"></i>
              <span style="font-size:13.5px;flex:1">Convolution</span>
              <span style="{{ convToggleStyle }}"><span style="{{ convKnobStyle }}"></span></span>
            </div>
            <div style="font-size:12px;color:var(--color-neutral-200)">room-correction-L+R.wav</div>
            <div style="font-size:11.5px;color:var(--color-neutral-500)">2 ch · 65 536 taps · +341.3 ms latency</div>
            <div style="display:flex;gap:8px">
              <span style="height:26px;padding:0 12px;display:flex;align-items:center;border:1px solid var(--color-neutral-800);border-radius:7px;font-size:12px;color:var(--color-neutral-200)">Replace…</span>
              <span style="height:26px;padding:0 12px;display:flex;align-items:center;border:1px solid var(--color-neutral-800);border-radius:7px;font-size:12px;color:var(--color-neutral-200)">Remove</span>
            </div>
          </div>
```

Replace with:

```html
          <div style="background:var(--color-neutral-900);border:1px solid var(--color-neutral-800);border-radius:10px;padding:14px 16px;display:flex;flex-direction:column;gap:10px">
            <div style="display:flex;align-items:center;gap:10px">
              <i class="ph ph-wave-sine" style="font-size:17px;color:var(--color-neutral-300)"></i>
              <span style="font-size:13.5px;flex:1">Convolution</span>
              <span style="{{ convToggleStyle }}"><span style="{{ convKnobStyle }}"></span></span>
            </div>
            <div style="font-size:12px;color:var(--color-neutral-200)">room-correction-L+R.wav</div>
            <div onClick="{{ convToggleExpand }}" style="display:flex;align-items:center;gap:8px;cursor:pointer">
              <div style="flex:1;font-size:11.5px;color:var(--color-neutral-500)">2 ch · 65 536 taps · +341.3 ms latency</div>
              <i class="ph ph-caret-down" style="font-size:11px;color:var(--color-neutral-500);flex:none;transform: {{ convCaretRot }}"></i>
            </div>
            <sc-if value="{{ convExpanded }}" hint-placeholder-val="{{ false }}">
              <div style="padding-top:6px;border-top:1px solid var(--color-neutral-800)">{{ convDetail }}</div>
            </sc-if>
            <div style="display:flex;gap:8px">
              <span style="height:26px;padding:0 12px;display:flex;align-items:center;border:1px solid var(--color-neutral-800);border-radius:7px;font-size:12px;color:var(--color-neutral-200)">Replace…</span>
              <span style="height:26px;padding:0 12px;display:flex;align-items:center;border:1px solid var(--color-neutral-800);border-radius:7px;font-size:12px;color:var(--color-neutral-200)">Remove</span>
            </div>
          </div>
```

- [ ] **Step 5: Add `convExpanded`/`convToggleExpand`/`convCaretRot`/`convDetail` to the final `renderVals()` return object**

Find:

```js
      chips, fxCards, mixApps, mixOuts, profiles, advFeatures,
      themes: this.themes(), miniCurveSvg: this.miniCurve(),
      goPresets: () => this.setState({page:'presets'}),
      goEffects: () => this.setState({page:'effects'}),
      ...this.tourVals(), ...this.simpleVals(),
      convToggleStyle: toggleOn, convKnobStyle: knobOn
    };
```

Replace with:

```js
      chips, fxCards, mixApps, mixOuts, profiles, advFeatures,
      themes: this.themes(), miniCurveSvg: this.miniCurve(),
      goPresets: () => this.setState({page:'presets'}),
      goEffects: () => this.setState({page:'effects'}),
      ...this.tourVals(), ...this.simpleVals(),
      convToggleStyle: toggleOn, convKnobStyle: knobOn,
      convExpanded: this.state.fxExpanded === '__conv__',
      convToggleExpand: () => this.setState({fxExpanded: this.state.fxExpanded === '__conv__' ? null : '__conv__'}),
      convCaretRot: this.state.fxExpanded === '__conv__' ? 'rotate(180deg)' : 'rotate(0deg)',
      convDetail: this.effectDetail('ir')
    };
```

- [ ] **Step 6: Visual check — expand two different cards**

```bash
cd /home/nyverino/resonance-mockups/resonance-overhaul
python3 -c "
from playwright.sync_api import sync_playwright
import os
with sync_playwright() as p:
    browser = p.chromium.launch(executable_path='/usr/bin/chromium', args=['--no-sandbox'])
    page = browser.new_page(viewport={'width':1400,'height':900})
    page.goto('file://' + os.path.abspath('Resonance Overhaul.dc.html'), wait_until='networkidle', timeout=30000)
    page.wait_for_timeout(1200)
    page.get_by_text('Effects', exact=True).first.click()
    page.wait_for_timeout(400)
    page.get_by_text('Restores the sparkle', exact=False).first.click()
    page.wait_for_timeout(300)
    page.screenshot(path='/tmp/task5-fidelity-expanded.png')
    page.get_by_text('Restores the sparkle', exact=False).first.click()
    page.get_by_text('Stereo field width, from mono-safe', exact=False).first.click()
    page.wait_for_timeout(300)
    page.screenshot(path='/tmp/task5-surround-expanded.png')
    browser.close()
"
```

Expected: `/tmp/task5-fidelity-expanded.png` shows a small rising curve under the Fidelity card's description; `/tmp/task5-surround-expanded.png` shows the L/R width bar under Surround, with Fidelity collapsed again. Open both and confirm.

---

### Task 6: Mixer page — per-channel frequency-response overlay

**Files:**
- Modify: `Resonance Overhaul.dc.html`

**Interfaces:**
- Consumes: `state.chVisible` (added in Task 3 Step 1), `bands()`, `var(--color-highlight)`.
- Produces: `channelLegend`, `channelCurveSvg` — new derived props consumed only by the Mixer page template.

- [ ] **Step 1: Add `channelVals()`/`channelCurve()` methods**

Find (anchor right after `miniCurve()`'s closing brace, before `tourVals()`):

```js
  tourVals() {
```

Replace with (inserts the two new methods before `tourVals`, which is otherwise unchanged):

```js
  channelVals() {
    const { chVisible } = this.state;
    const labels = [' FL', ' FR'];
    const channelLegend = labels.map((label,i)=>({
      label,
      toggle: () => { const v = chVisible.slice(); v[i] = !v[i]; this.setState({chVisible: v}); },
      dotCol: chVisible[i] ? (i===0 ? 'var(--color-accent)' : 'var(--color-highlight)') : 'var(--color-neutral-600)',
      eyeIcon: chVisible[i] ? 'ph-fill ph-eye' : 'ph ph-eye-slash',
      style: {display:'flex',alignItems:'center',gap:'6px',height:'26px',padding:'0 4px',border:'none',background:'transparent',fontSize:'12px',cursor:'pointer',fontFamily:'inherit',
        color: chVisible[i] ? 'var(--color-neutral-200)' : 'var(--color-neutral-500)'}
    }));
    return { channelLegend, channelCurveSvg: this.channelCurve(chVisible) };
  }

  channelCurve(visible) {
    const W = 700, H = 120, FS = 96000, DBR = 14;
    const scales = [1, 0.85];
    const bandsArr = this.bands();
    const X = f => W * Math.log(f/20) / Math.log(1000);
    const Y = db => H/2 - db * (H/2 - 8) / DBR;
    const magDbFn = scale => {
      const cs = bandsArr.map(b => {
        const g = b.g*scale;
        const A = Math.pow(10,g/40), w0 = 2*Math.PI*b.f/FS, cw = Math.cos(w0), sw = Math.sin(w0);
        const al = sw/(2*b.q), sA = Math.sqrt(A), k = 2*sA*al;
        if (b.t==='PK') return [1+al*A,-2*cw,1-al*A,1+al/A,-2*cw,1-al/A];
        if (b.t==='LS') return [A*((A+1)-(A-1)*cw+k),2*A*((A-1)-(A+1)*cw),A*((A+1)-(A-1)*cw-k),(A+1)+(A-1)*cw+k,-2*((A-1)+(A+1)*cw),(A+1)+(A-1)*cw-k];
        return [A*((A+1)+(A-1)*cw+k),-2*A*((A-1)+(A+1)*cw),A*((A+1)+(A-1)*cw-k),(A+1)-(A-1)*cw+k,2*((A-1)-(A+1)*cw),(A+1)-(A-1)*cw-k];
      });
      return f => {
        const w = 2*Math.PI*f/FS, c1=Math.cos(w),s1=Math.sin(w),c2=Math.cos(2*w),s2=Math.sin(2*w);
        let db = 0;
        for (const [b0,b1,b2,a0,a1,a2] of cs) {
          const nr=b0+b1*c1+b2*c2, ni=b1*s1+b2*s2, dr=a0+a1*c1+a2*c2, di=a1*s1+a2*s2;
          db += 10*Math.log10((nr*nr+ni*ni)/(dr*dr+di*di));
        }
        return db;
      };
    };
    const path = fn => { let d=''; for(let i=0;i<=180;i++){const f=20*Math.pow(1000,i/180); d+=(i?'L':'M')+X(f).toFixed(1)+' '+Y(fn(f)).toFixed(1);} return d; };
    const e = React.createElement, kids = [];
    kids.push(e('line',{key:'z',x1:0,y1:Y(0),x2:W,y2:Y(0),stroke:'var(--color-neutral-800)',strokeWidth:1}));
    const colors = ['var(--color-accent)','var(--color-highlight)'];
    scales.forEach((scale,i)=>{
      if (!visible[i]) return;
      kids.push(e('path',{key:'c'+i,d:path(magDbFn(scale)),stroke:colors[i],strokeWidth:i===0?2:1.6,strokeDasharray:i===0?'none':'5 4',fill:'none',opacity:0.95}));
    });
    return e('svg',{width:'100%',height:'100%',viewBox:`0 0 ${W} ${H}`,preserveAspectRatio:'none',style:{position:'absolute',inset:0,display:'block'}},kids);
  }

  tourVals() {
```

- [ ] **Step 2: Add a full-width "PER-CHANNEL RESPONSE" card to the Mixer page, gated to Advanced**

Find:

```html
          <sc-if value="{{ isAdvanced }}" hint-placeholder-val="{{ true }}">
            <div style="background:var(--color-neutral-900);border:1px solid var(--color-neutral-800);border-radius:10px;flex:1">
              <div style="height:42px;display:flex;align-items:center;justify-content:space-between;padding:0 16px;border-bottom:1px solid var(--color-neutral-800)">
                <span style="font-size:11px;letter-spacing:1.4px;color:var(--color-neutral-400)">CHANNELS</span>
                <span style="font-size:11px;color:var(--color-neutral-500)">2 ch · FL FR</span>
              </div>
              <div style="padding:12px 16px;display:flex;flex-direction:column;gap:12px">
                <div style="display:flex;align-items:center;gap:10px">
                  <span style="width:34px;height:19px;border-radius:10px;background:var(--color-neutral-800);position:relative"><span style="position:absolute;top:2.5px;left:2.5px;width:14px;height:14px;border-radius:50%;background:var(--color-neutral-400)"></span></span>
                  <span style="font-size:12.5px">Swap L / R</span>
                </div>
                <div style="display:flex;align-items:center;gap:10px">
                  <span style="width:34px;height:19px;border-radius:10px;background:var(--color-neutral-800);position:relative"><span style="position:absolute;top:2.5px;left:2.5px;width:14px;height:14px;border-radius:50%;background:var(--color-neutral-400)"></span></span>
                  <span style="font-size:12.5px">Per-channel EQ</span>
                  <span style="font-size:11px;color:var(--color-neutral-500)">aim bands at FL / FR only</span>
                </div>
              </div>
            </div>
          </sc-if>
        </div>
      </div>
      </sc-if>

      <!-- ── PRESETS page ── -->
```

Replace with:

```html
          <sc-if value="{{ isAdvanced }}" hint-placeholder-val="{{ true }}">
            <div style="background:var(--color-neutral-900);border:1px solid var(--color-neutral-800);border-radius:10px;flex:1">
              <div style="height:42px;display:flex;align-items:center;justify-content:space-between;padding:0 16px;border-bottom:1px solid var(--color-neutral-800)">
                <span style="font-size:11px;letter-spacing:1.4px;color:var(--color-neutral-400)">CHANNELS</span>
                <span style="font-size:11px;color:var(--color-neutral-500)">2 ch · FL FR</span>
              </div>
              <div style="padding:12px 16px;display:flex;flex-direction:column;gap:12px">
                <div style="display:flex;align-items:center;gap:10px">
                  <span style="width:34px;height:19px;border-radius:10px;background:var(--color-neutral-800);position:relative"><span style="position:absolute;top:2.5px;left:2.5px;width:14px;height:14px;border-radius:50%;background:var(--color-neutral-400)"></span></span>
                  <span style="font-size:12.5px">Swap L / R</span>
                </div>
                <div style="display:flex;align-items:center;gap:10px">
                  <span style="width:34px;height:19px;border-radius:10px;background:var(--color-neutral-800);position:relative"><span style="position:absolute;top:2.5px;left:2.5px;width:14px;height:14px;border-radius:50%;background:var(--color-neutral-400)"></span></span>
                  <span style="font-size:12.5px">Per-channel EQ</span>
                  <span style="font-size:11px;color:var(--color-neutral-500)">aim bands at FL / FR only</span>
                </div>
              </div>
            </div>
          </sc-if>
        </div>
        <sc-if value="{{ isAdvanced }}" hint-placeholder-val="{{ true }}">
        <div style="grid-column:1/-1;background:var(--color-neutral-900);border:1px solid var(--color-neutral-800);border-radius:10px">
          <div style="height:42px;display:flex;align-items:center;justify-content:space-between;padding:0 16px;border-bottom:1px solid var(--color-neutral-800)">
            <span style="font-size:11px;letter-spacing:1.4px;color:var(--color-neutral-400)">PER-CHANNEL RESPONSE</span>
            <span style="font-size:11px;color:var(--color-neutral-500)">what each channel's EQ is doing right now</span>
          </div>
          <div style="padding:12px 16px;display:flex;flex-direction:column;gap:10px">
            <div style="height:120px;position:relative;background:var(--color-bg);border:1px solid var(--color-neutral-800);border-radius:8px;overflow:hidden">{{ channelCurveSvg }}</div>
            <div style="display:flex;gap:16px">
              <sc-for list="{{ channelLegend }}" as="ch" hint-placeholder-count="2">
                <button onClick="{{ ch.toggle }}" style="{{ ch.style }}" style-hover="color: var(--color-accent-200)">
                  <i class="{{ ch.eyeIcon }}" style="font-size:13px;color: {{ ch.dotCol }}"></i>{{ ch.label }}
                </button>
              </sc-for>
            </div>
          </div>
        </div>
        </sc-if>
      </div>
      </sc-if>

      <!-- ── PRESETS page ── -->
```

- [ ] **Step 3: Spread `channelVals()` into `renderVals()`'s return object**

Find:

```js
      ...this.tourVals(), ...this.simpleVals(),
      convToggleStyle: toggleOn, convKnobStyle: knobOn,
```

Replace with:

```js
      ...this.tourVals(), ...this.simpleVals(), ...this.channelVals(),
      convToggleStyle: toggleOn, convKnobStyle: knobOn,
```

- [ ] **Step 4: Visual check — both channels, then FR hidden**

```bash
cd /home/nyverino/resonance-mockups/resonance-overhaul
python3 -c "
from playwright.sync_api import sync_playwright
import os
with sync_playwright() as p:
    browser = p.chromium.launch(executable_path='/usr/bin/chromium', args=['--no-sandbox'])
    page = browser.new_page(viewport={'width':1400,'height':900})
    page.goto('file://' + os.path.abspath('Resonance Overhaul.dc.html'), wait_until='networkidle', timeout=30000)
    page.wait_for_timeout(1200)
    page.get_by_text('Mixer', exact=True).first.click()
    page.wait_for_timeout(400)
    page.screenshot(path='/tmp/task6-mixer-both.png')
    page.get_by_text('FR', exact=True).first.click()
    page.wait_for_timeout(300)
    page.screenshot(path='/tmp/task6-mixer-fl-only.png')
    browser.close()
"
```

Expected: `/tmp/task6-mixer-both.png` shows two overlapping curves (solid accent FL, dashed highlight-colored FR) under a PER-CHANNEL RESPONSE card below Applications/Outputs/Channels; `/tmp/task6-mixer-fl-only.png` shows only the solid FL curve, FR's legend button dimmed. Open both and confirm.

---

### Task 7: Presets page — Recent Activity panel

**Files:**
- Modify: `Resonance Overhaul.dc.html`

**Interfaces:**
- Consumes: none new (static sample data, consistent with the rest of the mockup's fake `DaemonState`).
- Produces: `recentChanges`, `hasRecentChanges`, `noRecentChanges`.

- [ ] **Step 1: Add `recentActivityVals()`**

Find (anchor right after `channelCurve()`'s closing brace, before `tourVals()`):

```js
  tourVals() {
```

Replace with:

```js
  recentActivityVals() {
    const changes = [
      ['Band 1 gain','+3.2 dB','+4.7 dB'],
      ['Preamp','−6.0 dB','−6.9 dB'],
      ['Band 6 freq','3.9 kHz','4.1 kHz']
    ].map(([label,from,to])=>({label,from,to}));
    const hasRecentChanges = changes.length > 0;
    return { recentChanges: changes, hasRecentChanges, noRecentChanges: !hasRecentChanges };
  }

  tourVals() {
```

(Task 6 Step 1 already inserted `channelVals()`/`channelCurve()` directly before `tourVals()`, so by this point in the file `tourVals() {` is preceded by `channelCurve()`'s closing brace — this `Find` is unique and unambiguous. The net effect of both tasks' inserts is `miniCurve() → channelVals() → channelCurve() → recentActivityVals() → tourVals()`.)

- [ ] **Step 2: Add the Recent Activity card to the Presets page**

Find:

```html
          <div style="background:var(--color-neutral-900);border:1px solid var(--color-neutral-800);border-radius:10px;padding:12px 16px;display:flex;align-items:center;gap:12px">
            <div style="display:flex;border:1px solid var(--color-neutral-800);border-radius:8px;overflow:hidden;flex:none">
              <span style="height:30px;padding:0 16px;display:flex;align-items:center;font-size:12px;background:color-mix(in srgb, var(--color-accent) 16%, transparent);color:var(--color-accent-200)">A</span>
              <span style="height:30px;padding:0 16px;display:flex;align-items:center;font-size:12px;color:var(--color-neutral-400)">B</span>
            </div>
            <div style="min-width:0">
              <div style="font-size:12.5px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">A/B compare</div>
              <div style="font-size:11px;color:var(--color-neutral-500);white-space:nowrap;overflow:hidden;text-overflow:ellipsis">A = current edits · B = saved profile · hold Space to hear B</div>
            </div>
          </div>
          <sc-if value="{{ isAdvanced }}" hint-placeholder-val="{{ true }}">
```

Replace with:

```html
          <div style="background:var(--color-neutral-900);border:1px solid var(--color-neutral-800);border-radius:10px;padding:12px 16px;display:flex;align-items:center;gap:12px">
            <div style="display:flex;border:1px solid var(--color-neutral-800);border-radius:8px;overflow:hidden;flex:none">
              <span style="height:30px;padding:0 16px;display:flex;align-items:center;font-size:12px;background:color-mix(in srgb, var(--color-accent) 16%, transparent);color:var(--color-accent-200)">A</span>
              <span style="height:30px;padding:0 16px;display:flex;align-items:center;font-size:12px;color:var(--color-neutral-400)">B</span>
            </div>
            <div style="min-width:0">
              <div style="font-size:12.5px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">A/B compare</div>
              <div style="font-size:11px;color:var(--color-neutral-500);white-space:nowrap;overflow:hidden;text-overflow:ellipsis">A = current edits · B = saved profile · hold Space to hear B</div>
            </div>
          </div>
          <div style="background:var(--color-neutral-900);border:1px solid var(--color-neutral-800);border-radius:10px">
            <div style="height:42px;display:flex;align-items:center;justify-content:space-between;padding:0 16px;border-bottom:1px solid var(--color-neutral-800)">
              <span style="font-size:11px;letter-spacing:1.4px;color:var(--color-neutral-400)">RECENT ACTIVITY</span>
              <span style="font-size:11px;color:var(--color-neutral-500)">since last save</span>
            </div>
            <sc-if value="{{ hasRecentChanges }}" hint-placeholder-val="{{ true }}">
            <div style="padding:10px 16px;display:flex;flex-direction:column">
              <sc-for list="{{ recentChanges }}" as="rc" hint-placeholder-count="3">
                <div style="display:flex;align-items:center;gap:10px;padding:7px 0;border-bottom:1px solid var(--color-neutral-800)">
                  <span style="font-size:12px;color:var(--color-neutral-200);flex:1;min-width:0;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">{{ rc.label }}</span>
                  <span style="font-size:11.5px;color:var(--color-neutral-500);font-family:ui-monospace,monospace;white-space:nowrap">{{ rc.from }}</span>
                  <i class="ph ph-arrow-right" style="font-size:10px;color:var(--color-neutral-600)"></i>
                  <span style="font-size:11.5px;color:var(--color-accent-200);font-family:ui-monospace,monospace;white-space:nowrap">{{ rc.to }}</span>
                </div>
              </sc-for>
            </div>
            </sc-if>
            <sc-if value="{{ noRecentChanges }}" hint-placeholder-val="{{ false }}">
            <div style="padding:14px 16px;font-size:12px;color:var(--color-neutral-500)">No changes since last save.</div>
            </sc-if>
          </div>
          <sc-if value="{{ isAdvanced }}" hint-placeholder-val="{{ true }}">
```

- [ ] **Step 3: Spread `recentActivityVals()` into `renderVals()`'s return object**

Find:

```js
      ...this.tourVals(), ...this.simpleVals(), ...this.channelVals(),
      convToggleStyle: toggleOn, convKnobStyle: knobOn,
```

Replace with:

```js
      ...this.tourVals(), ...this.simpleVals(), ...this.channelVals(), ...this.recentActivityVals(),
      convToggleStyle: toggleOn, convKnobStyle: knobOn,
```

- [ ] **Step 4: Visual check**

```bash
cd /home/nyverino/resonance-mockups/resonance-overhaul
chromium --headless --disable-gpu --no-sandbox --hide-scrollbars \
  --window-size=1400,900 --screenshot="/tmp/task7-presets.png" \
  --virtual-time-budget=8000 --run-all-compositor-stages-before-draw \
  "file://$(pwd)/Resonance Overhaul.dc.html"
```

This lands on the Equalize page by default — the state starts on `page:'eq'`, so navigate first:

```bash
python3 -c "
from playwright.sync_api import sync_playwright
import os
with sync_playwright() as p:
    browser = p.chromium.launch(executable_path='/usr/bin/chromium', args=['--no-sandbox'])
    page = browser.new_page(viewport={'width':1400,'height':900})
    page.goto('file://' + os.path.abspath('/home/nyverino/resonance-mockups/resonance-overhaul/Resonance Overhaul.dc.html'), wait_until='networkidle', timeout=30000)
    page.wait_for_timeout(1200)
    page.get_by_text('Presets', exact=True).first.click()
    page.wait_for_timeout(400)
    page.screenshot(path='/tmp/task7-presets.png')
    browser.close()
"
```

Expected: `/tmp/task7-presets.png` shows a new "RECENT ACTIVITY" card between A/B compare and DEVICE → PROFILE, listing 3 from→to rows. Open and confirm.

---

### Task 8: Full verification pass — every page, several themes, plus the untouched baseline

**Files:** none (read-only verification task).

**Interfaces:** none.

- [ ] **Step 1: Screenshot all 5 pages under the default theme**

```bash
cd /home/nyverino/resonance-mockups/resonance-overhaul
python3 -c "
from playwright.sync_api import sync_playwright
import os
pages = ['Equalize','Effects','Mixer','Presets','Setup']
with sync_playwright() as p:
    browser = p.chromium.launch(executable_path='/usr/bin/chromium', args=['--no-sandbox'])
    page = browser.new_page(viewport={'width':1400,'height':900})
    page.goto('file://' + os.path.abspath('Resonance Overhaul.dc.html'), wait_until='networkidle', timeout=30000)
    page.wait_for_timeout(1200)
    for name in pages:
        page.get_by_text(name, exact=True).first.click()
        page.wait_for_timeout(400)
        page.screenshot(path=f'/tmp/final-default-{name}.png')
    browser.close()
"
```

Expected: 5 screenshots, all rendering without visual glitches (no missing icons, no unreadable text, no layout overflow). Open each and confirm.

- [ ] **Step 2: Screenshot the Equalize page under each of the 7 themes**

```bash
cd /home/nyverino/resonance-mockups/resonance-overhaul
python3 -c "
from playwright.sync_api import sync_playwright
import os
themes = ['Native Dark','Native Light','Breeze Dark','Gruvbox','Nord','Matrix','Light']
with sync_playwright() as p:
    browser = p.chromium.launch(executable_path='/usr/bin/chromium', args=['--no-sandbox'])
    page = browser.new_page(viewport={'width':1400,'height':900})
    page.goto('file://' + os.path.abspath('Resonance Overhaul.dc.html'), wait_until='networkidle', timeout=30000)
    page.wait_for_timeout(1200)
    for th in themes:
        page.get_by_text('Setup', exact=True).first.click()
        page.wait_for_timeout(300)
        page.get_by_text(th, exact=True).first.click()
        page.wait_for_timeout(300)
        page.get_by_text('Equalize', exact=True).first.click()
        page.wait_for_timeout(400)
        slug = th.lower().replace(' ','-')
        page.screenshot(path=f'/tmp/final-theme-{slug}.png')
    browser.close()
"
```

Expected: 7 screenshots, each with a visibly distinct background/accent/curve/legend combination matching that theme's palette (Nord = blue-teal on cool dark grey, Gruvbox = warm aqua on warm dark brown, Matrix = green-on-near-black, Light = the only bright/white-background one, etc.). Open all 7 and confirm none of them still show the original purple curve.

- [ ] **Step 3: Confirm `Current UI (Recreation).dc.html` is untouched**

```bash
diff <(unzip -p "/home/nyverino/Documents/resonance/Resonance GUI overhaul.zip" "Current UI (Recreation).dc.html") "/home/nyverino/resonance-mockups/resonance-overhaul/Current UI (Recreation).dc.html"
```

Expected: no output (files identical).

- [ ] **Step 4: Report back**

Summarize, for the user: which screenshots were reviewed, any visual issues spotted and whether they were fixed inline, and the final path (`/home/nyverino/resonance-mockups/resonance-overhaul/Resonance Overhaul.dc.html`) to open for further hands-on review.

---

## Out of scope (reiterated from the design doc)

- No `resonance-gui`/`theme.rs`/Rust changes.
- No live in-browser OKLCH computation — theme blocks are precomputed and static.
- `Current UI (Recreation).dc.html` is not touched (verified in Task 8).
- No new real DSP/IPC capability — the per-channel overlay and recent-activity panel are UI-only additions over fake sample data.
- Deciding when/how to port this mockup into egui — out of scope for this plan entirely.
