# EZD Studio

A macOS app for opening DXF drawings and EzCad `.ezd` jobs, checking them on a 110 mm field, and saving an `.ezd`.

The app is a file tool. It reads and writes job files. It does not talk to a laser control card, and it does not replace EzCad on the machine.

```shell
cd ezd-studio
cargo run --release
```

Open accepts `.dxf` and `.ezd`. Save writes `.ezd`. Keyboard shortcuts are Command-O and Command-S.

DXF import keeps lines, polylines, arcs, circles, ellipses, and splines, and it expands inserted blocks. Solid hatches are not copied: in these files the hatch repeats the same outlines, and tracing both would mark the drawing twice. The result is centered on the field.

Pen speed is mm/s, power is percent, and frequency is kHz. Those values are stored in EzCad's 256-pen table. The field size in the window is a view guide. Saving does not write that size into the file.

This Mac cannot run EzCad, so open a newly saved `.ezd` in EzCad 2 on Windows to confirm the machine accepts it. The vector section uses an identity Huffman table. Its two checksum words are CRC-16/X-25 of the raw object bytes and of the first 16 header bytes. A file EzCad saved matches that algorithm. Windows has not yet reopened a file written after the checksums were filled in.

The crate license in `Cargo.toml` is MIT. Rust 1.80 or newer is required. This tree was built with rustc 1.99.

## Documentation

| Guide | What it is for |
| --- | --- |
| [Architecture](docs/architecture.md) | Modules, the shared document, and how open and save move data |
| [EZD format](docs/ezd-format.md) | Byte layout of the `.ezd` this program reads and writes |
| [DXF import](docs/dxf-import.md) | Which entities become mark paths, and how they are placed |
| [Continuing the work](docs/continuing.md) | Tests, known limits, and the next changes that fit this design |

## Layout

```text
ezd-studio/
  Cargo.toml
  src/lib.rs          public API: open_drawing, read_ezd, read_dxf, write_ezd
  src/geom.rs         Document, PathObj, Contour, Pen
  src/dxf.rs          ASCII DXF import
  src/ezd/read.rs     .ezd reader
  src/ezd/write.rs    .ezd writer
  src/ezd/mod.rs      Huffman coder and the byte cursor
  src/ezd/pen_template.bin
  src/main.rs         eframe window
  docs/
```

`src/ezd/pen_template.bin` is the raw pen-0 record from a real EzCad 2.14 job. The writer copies it 256 times and patches color, name, passes, speed, power, and frequency. Do not regenerate it by hand.
