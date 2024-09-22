# quer
<img src="https://raw.githubusercontent.com/TJ9867/quer/main/rsrc/icon/quer-icon-512x512.png"
     style="display:block;float:none;margin-left:auto;margin-right:auto;width:50%">


A simple data finder application, written in Rust.

## Overview of Features
- Quick, multithreaded search
- Search for arbitrary regex supported by the `regex` crate
- Search for a wide variety hex patterns supported by the `regex` crate (WIP)
- Restrict search to only matches at a specific alignment
- Append results of multiple searches (configurable)
- Export results for a given file to an ImHex bookmarks file (aka `.imhexbm`)
- Export results to CSV (TODO)
- Preview file contents at the match
- Stable sorting of arbitrary columns
- Copy almost any value in almost any format
- Restrict search to X number of hits per file (configurable)
- Restrict search of hidden files (configurable)
- Memory of previous search regices

## Usage
On your favored platform:
```bash
git clone https://github.com/TJ9867/quer.git
cd quer
cargo run
```

![main quer GUI](https://raw.githubusercontent.com/TJ9867/quer/main/rsrc/main_gui.png)
---
## Demo
![main quer GUI](https://raw.githubusercontent.com/TJ9867/quer/main/rsrc/example_usage.gif)
---

## Explanation
A simple cross-platform application to help you find/keep track of binary and text in a non-trivial number of files.

