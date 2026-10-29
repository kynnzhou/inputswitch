# InputSwitch

<!-- impeccable:product-schema 1 -->

## Platform

web

The control panel uses plain HTML, CSS and JavaScript inside a Rust desktop application (Windows WebView2 and macOS WKWebView).

## Product Purpose

Switch a monitor between computers without its physical buttons; share keyboard, mouse, clipboard text and files between paired computers on the local network.

## Operating Context

The user confirmed that one monitor with multiple computers and multiple computers with separate monitors are equally important. The redesigned home must offer both views. This request delivers an interactive design prototype, with the main functions visible without scrolling in a normal desktop window.

## Capabilities and Constraints

- Repository evidence: DDC/CI input switching, peer-assisted switching, signed pairing, keyboard and mouse sharing, four directional neighbor positions, follow-display links, file and folder transfer, clipboard synchronization, hotkeys, startup, recovery, watchdog and software updates.
- A connection's position determines its sharing behavior; the home scene selector is a view, not a new global sharing mode. Both kinds of connection can coexist.
- Keep the main switch action visible and clickable even when switching is unavailable, and explain why. Keep the three maintenance actions visible and flat. The main action is the only solid blue button on the unobscured panel.
- Status must distinguish a request sent from an operation confirmed. No clickable mouse-reclaim control: the real escape path is Ctrl+Alt+Esc or physical local input, depending on the session.
- Production defaults are 820 by 840 logical pixels, with a minimum of 600 by 520 and height constrained by the user's display. The prototype must validate compact widths and heights as well as its preferred size.
- Device names, network addresses, activity and transfers in the prototype are illustrative. The prototype does not call the production API or change the user's hardware or files.

## Brand Commitments

Retain the InputSwitch name and existing icon. Chinese is the primary interface language. The user's requested qualities are simplicity, friendly interaction, clear feedback and purposeful animation.

## Evidence on Hand

README.md, CONTRIBUTING.md, assets/panel/, src/panelwin.rs and design/native-compare/ describe the shipped behavior and incumbent interface. The earlier design files are historical evidence, not the visual specification for this redesign.

## Product Principles

- Keep the current picture, keyboard owner and next useful action easy to find.
- Give single-screen switching and multiple-screen sharing equal access.
- Reveal infrequent configuration where it is needed, without making routine work scroll.
- Make pending, confirmed, unavailable and failed operations distinguishable.

## Open Decisions

The preferred new default window dimensions and the final appearance remain design proposals for review. Production integration is outside this prototype deliverable.
