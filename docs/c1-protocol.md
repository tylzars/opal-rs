# Opal C1 USB interface

Observed on a C1 running its built-in firmware.

## Identity
Vendor `03e7`, product `f63d`, strings `Opal` / `Opal C1`. USB 3.2, self-powered. The C1 is built on an Intel
Movidius Myriad X; `f63d` is the Myriad X ID for an application started from flash.

## Interfaces
| # | Class | Endpoints | Use |
| --- | --- | --- | --- |
| 0 | Vendor | bulk `0x01` out, `0x81` in | XLink |
| 1 | Video control (UVC 1.00) | | camera terminal 1, processing unit 3, extension unit 4, output terminal 2 |
| 2 | Video streaming | bulk `0x82` in | NV12 1920x1080 at 60 fps only |
| 3 | Audio control | | |
| 4 | Audio streaming | iso `0x83` in | microphone |

## UVC controls
`GET_*` requests (`bmRequestType` `0xA1`) to interface 1:

| Unit | Control | Selector | Result |
| --- | --- | --- | --- |
| PU 3 | Brightness | `0x02` | min 0, max 255, res 1, def 127 |
| PU 3 | White balance temperature | `0x0A` | min 2810, max 6500, res 1000, def 5800 |
| PU 3 | Gain | `0x04` | stall |
| CT 1 | AE mode, AE priority, exposure time, focus, auto focus | `0x02` to `0x08` | stall |
| CT 1 | Iris | `0x09` | stall |
| XU 4 | selectors 1 to 8 | | `GET_INFO` 3, `GET_LEN` 1 |
| XU 4 | selectors 9 to 64 | | `GET_INFO` 0, `GET_LEN` 0 |

`GET_CUR` returned 0 for every control queried, including ones with a non-zero default.

## Camera files
| File | Format |
| --- | --- |
| `depthai-device-openvino-2021.4-*.cmd` | Myriad X application image, starts with `MA2x` |
| `*.tlv` | Myriad X neural networks, blob magic `9709`, blob version 6.0 (OpenVINO 2020.4 to 2021.4) |
| `Opal_v0.12.bin`, `Opal582_v2.bin` | Sensor tuning |
