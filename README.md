# herald

<img src="img/herald.svg" align="right" width="150">

**Desktop notifications: small boxes at the top right, and nothing running while none arrive.**

![Rust](https://img.shields.io/badge/language-Rust-orange) ![Unlicense](https://img.shields.io/badge/license-Unlicense-green) ![Platform](https://img.shields.io/badge/platform-Linux-blue) ![Stay Amazing](https://img.shields.io/badge/Stay-Amazing-important)

herald answers on D-Bus as the desktop's notification service, the one `notify-send` and every program talk to. Each notification shows as a box at the top right, coloured by how urgent it is. Part of the [Fe₂O₃ Rust terminal suite](https://github.com/isene/fe2o3).

It replaces dunst. dunst held 21 MB here; herald holds 5 MB and wakes only when a notification arrives.

## Using it

Start it with your session, or let D-Bus start it on the first notification:

```ini
# ~/.local/share/dbus-1/services/org.freedesktop.Notifications.service
[D-BUS Service]
Name=org.freedesktop.Notifications
Exec=/path/to/herald
```

| Does | How |
|---|---|
| Close a box | Click it |
| Close the newest | `Ctrl+Space` |
| Close them all | `Ctrl+Shift+Space` |
| See the last 15 | `herald --history` |
| Quiet for a while | `herald --quiet` (50 minutes) or `herald --quiet 25`; run it again to end early |
| Look at it all | `herald` in a terminal: the service's state and every notification, `t` sends a test |

herald holds the two keys only while a box is up. The rest of the time they belong to your other programs.

During a quiet spell no boxes show, except critical ones. At the end one box lists what came in, and it stays until you click it. Bind the command to a key in your window manager, such as `bind Mod4+m exec herald --quiet` in [tile](https://github.com/isene/tile) (m for mute; frame keeps Mod4+n for night light). The spell costs one timer, for its end.

A notification with the hint `category` set to `red-alert` is a red alert: big red letters between two red bars, pulsing for four seconds, then lit until it is closed. [drain](https://github.com/isene/drain) sends one when the battery runs low:

```sh
notify-send -u critical -h string:category:red-alert "RED ALERT" "Battery at 9 %."
```

## Settings

`~/.heraldrc`, one `key = value` per line. Every key is optional:

| Key | Default | Means |
|---|---|---|
| `font`, `font_bold` | DejaVu Sans Mono | Font files |
| `font_size` | `15` | Letter size in pixels |
| `width` | `360` | Box width in pixels |
| `x`, `y` | `0`, `40` | Gap from the right edge and from the top |
| `gap`, `padding`, `frame` | `4`, `6`, `2` | Space between boxes, inside a box, and the frame |
| `low_fg`, `normal_fg`, `critical_fg` | `#3B7C87`, `#5B8234`, `#B7472A` | Text and frame colour |
| `low_bg`, `normal_bg`, `critical_bg` | `#191311` | Background |
| `low_timeout`, `normal_timeout`, `critical_timeout` | `4`, `6`, `8` | Seconds on screen; `0` stays until clicked |
| `skip` | none | A title to keep off the screen (it still goes in the history). Repeat for more |
| `shape` | `box` | `elbow` puts the title on a bar of the urgency's colour, joined by a round elbow to a bar down the left (the LCARS view in [fe2o3](https://github.com/isene/fe2o3)'s `views/` sets it) |
| `title_fg`, `text_fg` | `#000000`, the urgency's colour | With `shape = elbow`: the title on its bar, and the text below |

A sender can set its own time, or ask for a box that stays. A notification sent again with the same id changes its box in place.

## How it works

The D-Bus side runs on zbus and only hands notifications to the main loop. The main loop waits on a channel and wakes only when something arrives. It keeps a timer only while a box is on screen.

The letters are drawn by herald itself with ab_glyph, since some X servers (frame among them) carry no fonts. A letter is read from the font file the first time it is drawn, then kept.

## Install

```bash
cargo install --path .
```

## License

Public domain. Do what you like with it.
