# Material icons

Colour file-type and folder icons from [Material Icon Theme](https://github.com/material-extensions/vscode-material-icon-theme) v5.39.0 (MIT, Copyright (c) 2025 Material Extensions; text in assets/NOTICE), taken from the `icons/` folder of that tag. Drawn with `img()` through the asset source (`icons/material/<name>.svg`), mapped by name and extension in src/files/icons.rs.

- The 63 icons of uherdr's Resources/FileIcons were copied from there (the same tag): the languages, `folder-*` set and the generic ones (`document`, `tune`, `console`, ...).
- `folder.svg` is the one modified file, as in uherdr: the theme's `folder-base.svg` with its overlay shape removed and the folder colour set to the theme's default folder grey (`#90a4ae`).
- More file icons come unchanged from the same tag: nix, zig, svelte, astro, elixir, haskell, terraform, proto, editorconfig, prettier, eslint, tsconfig and others (see the listing).
- More `folder-*` icons come unchanged from the same tag.
- Every `folder-*-open.svg` (and `folder-open.svg`, from the grey `folder.svg`) is generated the way the theme's own build does (src/scripts/svg/generateOpenFolderIcons.ts): the closed icon with the path of `id="folder"` replaced by the theme's open-folder path. Nothing else changes.

Only plain SVG files that parse as XML with no script, event handler, external reference or DOCTYPE were kept. To add an icon, copy it from the same tag, add its name to the list in src/assets.rs (`material_names!`) and map it in src/files/icons.rs; the tests check the list against this folder.
