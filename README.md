# EZD Studio

A macOS app for opening DXF drawings and EzCad `.ezd` jobs, checking them on a 110 mm field, and saving an `.ezd`.

```shell
cd ezd-studio
cargo run --release
```

Open accepts `.dxf` and `.ezd`. Save writes `.ezd`.

DXF import keeps lines, polylines, arcs, circles, ellipses, and splines, and it expands inserted blocks. Solid hatches are not copied: in these files the hatch repeats the same outlines, and tracing both would mark the drawing twice. The result is centered on the field.

Pen speed is mm/s, power is percent, and frequency is kHz. Those values are stored in EzCad's 256-pen table.

This Mac cannot run EzCad, so open the saved `.ezd` in EzCad 2 on Windows to confirm the machine accepts it.
