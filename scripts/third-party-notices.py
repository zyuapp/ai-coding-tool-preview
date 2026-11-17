#!/usr/bin/env python3
"""Writes assets/legal/THIRD-PARTY-NOTICES.txt from Cargo.lock.

    python3 -I scripts/third-party-notices.py [--check] [--cua-driver <trycua/cua checkout>]

Lists every crate the app and its helpers link on the release targets (normal dependencies of the
workspace members; build and dev dependencies are left out), with the license texts each crate
ships, grouped so a text shared by many crates appears once. --check exits 1 when the file is stale.
--cua-driver does the same for the Linux Cua Driver, from a checkout of trycua/cua at the
cua-driver-rs-v<version> tag of the version scripts/package-linux.sh bundles, into
assets/legal/linux/CUA-RUST-DEPENDENCIES-LINUX.txt.
"""
import json
import os
import re
import subprocess
import sys
import textwrap

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
OUT = os.path.join(ROOT, "assets", "legal", "THIRD-PARTY-NOTICES.txt")
CUA_OUT = os.path.join(ROOT, "assets", "legal", "linux", "CUA-RUST-DEPENDENCIES-LINUX.txt")


def app_name():
    with open(os.path.join(ROOT, "crates", "core", "src", "identity.rs"), encoding="utf-8") as file:
        return re.search(r'pub const APP_NAME: &str = "([^"]+)";', file.read()).group(1)


APP = app_name()

# The Linux package's dictation helper is built with act_devices/onnx; macOS uses the Swift helper.
TARGETS = [
    ("aarch64-apple-darwin", []),
    ("x86_64-unknown-linux-gnu", ["act_devices/onnx"]),
    ("aarch64-unknown-linux-gnu", ["act_devices/onnx"]),
]

# The Linux Cua Driver release build (cd-rust-cua-driver.yml in trycua/cua): the cua-driver binary with
# cua-driver/portal-input.
CUA_TARGETS = [
    ("x86_64-unknown-linux-gnu", ["cua-driver/portal-input"]),
    ("aarch64-unknown-linux-gnu", ["cua-driver/portal-input"]),
]

LICENSE_FILE = re.compile(r"^(licen[cs]e|copying|copyright|notice|unlicense|patents)([-_.].*)?$", re.I)
COPYRIGHT_LINE = re.compile(r"^\s*(portions )?(copyright\b(?!\s+(notice|license|licence|owner|holder|and|or|statement|law|protection|in|to|of|the)\b)|(\(c\)|©)\s*\d)", re.I)
TEMPLATE_LINE = re.compile(r"\[yyyy\]|\{yyyy\}|<year>|\[name of copyright owner\]|\{name of copyright owner\}", re.I)

# C and C++ libraries compiled into the binaries from sources a -sys crate bundles.
# (crate, library, license, files relative to the crate, note)
BUNDLED_C = [
    ("onig_sys", "Oniguruma", "BSD-2-Clause", ["oniguruma/COPYING"], "Compiled from the sources onig_sys bundles."),
    ("libsqlite3-sys", "SQLite", "Public domain", [], "Compiled from the amalgamation libsqlite3-sys bundles. "
     "The SQLite authors have dedicated SQLite to the public domain: https://sqlite.org/copyright.html"),
    ("freetype-sys", "FreeType", "FTL OR GPL-2.0-or-later", ["freetype2/docs/FTL.TXT"],
     "Linux only. Linked from the system when its FreeType is new enough, else compiled from the sources "
     "freetype-sys bundles, used under the FreeType License. Portions of this software are copyright "
     "The FreeType Project (https://freetype.org). All rights reserved."),
]

CUA_BUNDLED_C = [
    ("rquickjs-sys", "QuickJS-ng", "MIT", ["quickjs/LICENSE"], "Compiled from the sources rquickjs-sys bundles."),
    ("zstd-sys", "Zstandard", "BSD-3-Clause OR GPL-2.0", ["zstd/LICENSE"],
     "Compiled from the sources zstd-sys bundles, used under the BSD-3-Clause license below."),
]

FILES = [
    ("AI-CODING-TOOL-MIT.txt", f"{APP}'s own license."),
    ("FONTS.txt", "The bundled fonts (SIL Open Font License 1.1)."),
    ("ICONS.txt", "Lucide, Bootstrap Icons and Simple Icons glyphs."),
    ("MERMAID-THIRD-PARTY-NOTICES.txt", "Mermaid and the JavaScript packages it bundles."),
    ("TEXTMATE-GRAMMARS.txt", "The TextMate grammars and tokenizer that colour code."),
    ("DICTATION-SWIFT-DEPENDENCIES.txt", "Swift packages the macOS dictation helper links."),
    ("CUA-MIT.txt", "The bundled Cua Driver."),
    ("CUA-THIRD-PARTY-NOTICES.md", "Code the Cua Driver derives from or embeds."),
    ("CUA-RUST-DEPENDENCIES.html", "Rust crates the macOS Cua Driver links."),
    ("INTER-OFL-1.1.txt", "The Inter font the Cua Driver's cursor overlay embeds."),
    ("MPL-2.0.txt", "The Mozilla Public License 2.0."),
    ("onnxruntime/", "ONNX Runtime, which the Linux dictation helper loads (Linux only)."),
    ("linux/CUA-RUST-DEPENDENCIES-LINUX.txt", "Rust crates the Linux Cua Driver links (Linux only)."),
    ("linux/APPIMAGE-RUNTIME.txt", "The AppImage runtime and the libraries it links (Linux only)."),
]

PARAGRAPHS = {
    "intro": f"{APP} is licensed under the MIT License. It is built with the open source software below. "
             "The files beside this one hold the notices for the rest of what it ships:",
    "model": "Speech model: dictation downloads NVIDIA's Parakeet TDT 0.6B v2 when asked, from Hugging Face: a Core ML "
             "conversion (FluidInference/parakeet-tdt-0.6b-v2-coreml) on macOS and an ONNX export "
             "(istupakov/parakeet-tdt-0.6b-v2-onnx) on Linux. The model is not part of the app. NVIDIA licenses it under "
             "the Creative Commons Attribution 4.0 International License (https://creativecommons.org/licenses/by/4.0/); "
             "see https://huggingface.co/nvidia/parakeet-tdt-0.6b-v2.",
    "crates": f"The crates below are compiled into the app and its helpers, each used under the license named beside it. "
              f"Where a crate offers a choice of licenses, {APP} uses it under the one whose text is given below. "
              f"Crates marked modified are changed copies; the NOTICE file in each one's folder under vendor/ in the "
              f"{APP} source describes the changes, and is reproduced at the end. Crates under the Mozilla Public "
              "License 2.0 are used unmodified; their source is available from crates.io at the version listed "
              "(https://crates.io/crates/<name>/<version>), and the license is in MPL-2.0.txt.",
}


def overview():
    title = f"{APP} open source notices"
    width = max(len(name) for name, _ in FILES) + 3
    out = [title, "=" * len(title), "", fill(PARAGRAPHS["intro"]), ""]
    out += [name.ljust(width) + what for name, what in FILES]
    out += ["", fill(PARAGRAPHS["model"]), "", "Rust crates", "-----------", "", fill(PARAGRAPHS["crates"]), ""]
    return "\n".join(out)


def fill(text):
    return textwrap.fill(text, 100, break_long_words=False, break_on_hyphens=False)


def metadata(target, features, cwd):
    command = ["cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", target]
    if features:
        command += ["--features", ",".join(features)]
    return json.loads(subprocess.run(command, cwd=cwd, check=True, capture_output=True, text=True).stdout)


def shipped_packages(targets, cwd=ROOT, root=None):
    """The crates the workspace members link, or with root, the crates that member links besides the
    workspace's own path crates."""
    chosen = {}
    for target, features in targets:
        data = metadata(target, features, cwd)
        packages = {package["id"]: package for package in data["packages"]}
        nodes = {node["id"]: node for node in data["resolve"]["nodes"]}
        members = set(data["workspace_members"])
        start = members if root is None else {package_id for package_id in members if packages[package_id]["name"] == root}
        seen, stack = set(), list(start)
        while stack:
            current = stack.pop()
            if current in seen:
                continue
            seen.add(current)
            for dep in nodes[current]["deps"]:
                if any(kind["kind"] is None for kind in dep["dep_kinds"]):
                    stack.append(dep["pkg"])
        own = members if root is None else {package_id for package_id in seen if packages[package_id]["source"] is None}
        for package_id in seen - own:
            chosen[package_id] = packages[package_id]
    return sorted(chosen.values(), key=lambda package: (package["name"], version_key(package["version"])))


def version_key(version):
    return [(0, int(part), "") if part.isdigit() else (1, 0, part) for part in re.split(r"[.+-]", version)]


def read(path):
    with open(path, encoding="utf-8", errors="replace") as file:
        lines = [line.rstrip() for line in file.read().replace("\r\n", "\n").replace("\r", "\n").split("\n")]
    return "\n".join(lines).strip("\n")


def license_files(package):
    folder = os.path.dirname(package["manifest_path"])
    files = [os.path.join(folder, name) for name in os.listdir(folder) if LICENSE_FILE.match(name) and os.path.isfile(os.path.join(folder, name))]
    licenses_dir = os.path.join(folder, "LICENSES")
    if os.path.isdir(licenses_dir):
        files += [os.path.join(parent, name) for parent, _, names in os.walk(licenses_dir) for name in names]
    if package.get("license_file"):
        files.append(os.path.join(folder, package["license_file"]))
    # A symlink packaged as its target's path (e.g. "../LICENSE-MIT") holds no text.
    return sorted(path for path in set(os.path.realpath(path) for path in files) if not re.fullmatch(r"\S{1,200}", read(path)))


def split_copyright(text):
    """The copyright lines a text names, and the rest of it."""
    held, body = [], []
    for line in text.split("\n"):
        if COPYRIGHT_LINE.match(line) and not TEMPLATE_LINE.search(line):
            held.append(line.strip())
        else:
            body.append(line)
    return held, re.sub(r"\n{3,}", "\n\n", "\n".join(body)).strip("\n")


# Preferred license when a crate offers a choice; the texts of the options not taken are left out.
PREFERENCE = ["MIT", "Apache-2.0", "BSD-3-Clause", "BSD-2-Clause", "ISC", "Zlib", "0BSD", "MIT-0", "Unlicense",
              "CC0-1.0", "BSL-1.0", "Unicode-3.0", "Unicode-DFS-2016", "CDLA-Permissive-2.0", "MPL-2.0"]

KINDS = [
    ("Apache-2.0", re.compile(r"apache license.{0,40}version 2\.0", re.I | re.S)),
    ("MIT", re.compile(r"permission is hereby granted, free of charge", re.I)),
    ("Zlib", re.compile(r"altered source versions must be plainly marked", re.I)),
    ("Unlicense", re.compile(r"this is free and unencumbered software", re.I)),
    ("BSD", re.compile(r"redistribution and use in source and binary forms", re.I)),
    ("ISC", re.compile(r"permission to use, copy, modify, and/or distribute this software for any", re.I)),
    ("GPL", re.compile(r"gnu (lesser |library )?general public license", re.I)),
    ("BSL-1.0", re.compile(r"boost software license", re.I)),
    ("CC0-1.0", re.compile(r"cc0 1\.0|creative commons legal code", re.I)),
    ("MPL-2.0", re.compile(r"mozilla public license", re.I)),
    ("Unicode", re.compile(r"unicode license|unicode, inc\.", re.I)),
]


def kind_of(text):
    head = text[:3000]
    found = [kind for kind, pattern in KINDS if pattern.search(head)]
    return found[0] if len(found) == 1 else None


def chosen_option(expression):
    """The licenses a crate is used under, or None for a grouped expression, whose texts are all kept."""
    if "(" in (expression or ""):
        return None
    options = [option.strip(" ()") for option in re.split(r"\bOR\b|/", expression or "")]
    terms = [[term.strip(" ()").split(" WITH ")[0] for term in re.split(r"\bAND\b", option)] for option in options if option]
    ranked = sorted(terms, key=lambda option: max(PREFERENCE.index(term) if term in PREFERENCE else 99 for term in option))
    return ranked[0] if ranked else []


def family(term):
    if term.startswith("BSD"):
        return "BSD"
    if term.startswith("Unicode"):
        return "Unicode"
    if term in ("ISC", "0BSD"):
        return "ISC"
    return term


def chosen_texts(package, files):
    """The license files for the license the crate is used under, plus its notices."""
    texts = [(path, read(path)) for path in files]
    option = chosen_option(package["license"])
    if option is None:
        return texts
    wanted = {family(term) for term in option}
    kept = []
    for path, text in texts:
        kind = kind_of(text)
        if os.path.basename(path).lower().startswith("notice") or kind is None or family(kind) in wanted:
            kept.append((path, text))
    return kept or texts


def strip_apache_appendix(text):
    start = text.find("APPENDIX: How to apply the Apache License")
    if start < 0:
        return text
    end = text.find("limitations under the License.", start)
    if end < 0:
        return text
    return (text[:start] + text[end + len("limitations under the License."):]).rstrip()


def vendored_folder(package):
    """The crate's folder relative to the repository when it is a copy under vendor/, else None."""
    if package["source"] is not None:
        return None
    return os.path.relpath(os.path.dirname(package["manifest_path"]), ROOT)


def is_modified(package):
    folder = os.path.dirname(package["manifest_path"])
    return any(name.lower().startswith("notice") for name in os.listdir(folder))


def label(package):
    return f'{package["name"]} {package["version"]}'


def render(packages, head, bundled):
    missing = []
    groups = {}
    notices = {}
    for package in packages:
        files = license_files(package)
        if not files:
            missing.append(package)
            continue
        for path, text in chosen_texts(package, files):
            if os.path.basename(path).lower().startswith("notice"):
                notices.setdefault(text, []).append(label(package))
                continue
            held, body = split_copyright(strip_apache_appendix(text))
            key = re.sub(r"\s+", " ", body).strip().lower()
            group = groups.setdefault(key, {"crates": [], "copyright": {}, "body": body})
            if label(package) not in group["crates"]:
                group["crates"].append(label(package))
            if held:
                group["copyright"].setdefault(label(package), []).extend(line for line in held if line not in group["copyright"].get(label(package), []))

    width = max(len(label(package)) for package in packages) + 2
    out = [head]
    for package in packages:
        license = package["license"] or "see its license file"
        folder = vendored_folder(package)
        suffix = f"  ({'modified, ' if is_modified(package) else ''}{folder})" if folder else ""
        out.append(f"{label(package).ljust(width)}{license}{suffix}")

    out += ["", "", "C libraries", "-----------", "",
            "These libraries are compiled in from sources the crates named beside them bundle.", ""]
    registry = {package["name"]: os.path.dirname(package["manifest_path"]) for package in packages}
    for crate, library, license, files, note in bundled:
        if crate not in registry:
            continue
        out += ["=" * 100, f"{library} (through {crate}), {license}", "-" * 100, note]
        for name in files:
            out += ["", read(os.path.join(registry[crate], name))]
        out.append("")

    out += ["", "License texts", "-------------", "",
            "Each text below applies to the crates named above it. Copyright lines follow the crate that holds them.", ""]
    for group in sorted(groups.values(), key=lambda group: (-len(group["crates"]), group["crates"][0])):
        out += ["=" * 100, wrap_names(group["crates"]), "-" * 100]
        for crate in group["crates"]:
            for line in group["copyright"].get(crate, []):
                out.append(f"{crate}: {line}")
        if group["copyright"]:
            out.append("")
        out += [group["body"], ""]

    if notices:
        out += ["", "NOTICE files", "------------", ""]
        for text, crates in notices.items():
            out += ["=" * 100, wrap_names(crates), "-" * 100, text, ""]

    if missing:
        out += ["", "Crates without a license text", "-----------------------------", "",
                "These crates publish no license text in their package. Each is used under the license named in the list",
                "above, whose standard text is given in this file for other crates, and is copyright the authors named here.", ""]
        for package in missing:
            authors = ", ".join(re.sub(r"\s*<[^>]*>", "", author) for author in package["authors"]) or "its contributors"
            out.append(f"{label(package)}: copyright {authors}. {package['repository'] or ''}".rstrip())
    return "\n".join(out).rstrip("\n") + "\n"


def wrap_names(names, width=100):
    lines, line = [], ""
    for name in names:
        piece = name if not line else ", " + name
        if len(line) + len(piece) > width:
            lines.append(line + ",")
            line = name
        else:
            line += piece
    return "\n".join(lines + [line])


PERMISSIVE = {"MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib", "Unicode-3.0", "Unicode-DFS-2016",
              "0BSD", "CC0-1.0", "MIT-0", "Unlicense", "BSL-1.0", "CDLA-Permissive-2.0", "MPL-2.0",
              "Apache-2.0 WITH LLVM-exception"}


def unclear(packages):
    """Crates whose license expression offers no permissive choice."""
    flagged = []
    for package in packages:
        expression = (package["license"] or "").replace("/", " OR ")
        options = [option.strip(" ()") for option in re.split(r"\bOR\b", expression)]
        if not any(all(term.strip(" ()") in PERMISSIVE for term in re.split(r"\bAND\b", option)) for option in options):
            flagged.append(f'{label(package)}: {package["license"] or package.get("license_file") or "no license"}')
    return flagged


def cua_head(checkout):
    with open(os.path.join(ROOT, "scripts", "package-linux.sh"), encoding="utf-8") as file:
        version = re.search(r"^cua_version=(\S+)$", file.read(), re.M).group(1)
    with open(os.path.join(checkout, "Cargo.toml"), encoding="utf-8") as file:
        found = re.search(r'^version = "([^"]+)"', file.read(), re.M).group(1)
    if found != version:
        sys.exit(f"The checkout is Cua Driver {found}; scripts/package-linux.sh bundles {version}.")
    title = f"Cua Driver {version} for Linux: Rust crates"
    intro = (f"The Linux Cua Driver that {APP} bundles is built by trycua/cua from its cua-driver-rs-v{version} tag "
             "(https://github.com/trycua/cua/tree/cua-driver-rs-v" + version + "/libs/cua-driver/rust) for x86_64 and "
             "arm64. The Cua Driver's own crates are under the MIT License in CUA-MIT.txt, with the code they derive "
             "from in CUA-THIRD-PARTY-NOTICES.md. It links the crates below, each used under the license named "
             "beside it; where a crate offers a choice, it is used under the one whose text is given below. Crates "
             "under the Mozilla Public License 2.0 are used unmodified; their source is available from crates.io at "
             "the version listed (https://crates.io/crates/<name>/<version>), and the license is in MPL-2.0.txt.")
    return "\n".join([title, "=" * len(title), "", fill(intro), ""])


def write(out, text, packages):
    for line in unclear(packages):
        print("check the license of " + line, file=sys.stderr)
    if "--check" in sys.argv[1:]:
        with open(out, encoding="utf-8") as file:
            if file.read() != text:
                print(f"{os.path.relpath(out, ROOT)} is stale; run scripts/third-party-notices.py", file=sys.stderr)
                sys.exit(1)
        return
    with open(out, "w", encoding="utf-8") as file:
        file.write(text)
    print(f"Wrote {os.path.relpath(out, ROOT)}: {len(packages)} crates, {len(text) // 1024} KB")


def main():
    args = sys.argv[1:]
    if "--cua-driver" in args:
        checkout = os.path.join(os.path.abspath(args[args.index("--cua-driver") + 1]), "libs", "cua-driver", "rust")
        packages = shipped_packages(CUA_TARGETS, checkout, "cua-driver")
        write(CUA_OUT, render(packages, cua_head(checkout), CUA_BUNDLED_C), packages)
        return
    packages = shipped_packages(TARGETS)
    write(OUT, render(packages, overview(), BUNDLED_C), packages)


if __name__ == "__main__":
    main()
