---
name: desktop-pet
description: "Make the user a new desktop pet from a description or a picture they sent: one character drawn in every pose Filo's desktop pet takes, with its motion data, saved among their characters (Settings › Desktop pet, Characters on App icon) and made the pet. Use whenever the user asks for a new pet, character or mascot for the desktop, or to redraw one."
---

# A new desktop pet

Filo's pet lives on the user's desktop, above every app, and takes a pose for what's going on: **idle** (resting, the default), **working** (replying or on a task), **waving** (a reminder is due, a reply just landed) and **sleeping** (late at night). A new pet is one character drawn in those four poses. Filo's `save_pet` tool cuts each out on a 400px square, sizes and places them, finds the eyes so the pet can blink, and saves the pet; your job is good, consistent pictures.

The pictures come from Filo's `generate_images` tool, which has the user's Codex draw them; the chat says Codex is drawing while it runs and shows each picture as Codex drew it when it returns, so you don't need to show them again. Each one costs the user money: make exactly four, and remake a pose at most once, only when the checks below say so. If `generate_images` says it can't make pictures (Codex missing or not signed in), stop and tell the user what it said they need; don't try other ways.

## 1. Set up

- Pick a short name for the pet from the request (at most 40 characters), like "Astronaut cat".
- Make a new folder for the pictures with the shell: `mktemp -d -t filo-pet`. Below, `DIR` is the folder it printed.
- Load `generate_images` and `save_pet` (Filo's tools) if you don't have them yet.

## 2. The resting pose

From a description, call `generate_images` with one image, `path` `DIR/idle.png`, and this prompt with the character filled in:

> Cute chibi mascot, premium glossy 3D vinyl toy look with cobalt blue accents, soft studio lighting, friendly rounded proportions, one character, full body, centred, nothing cropped. The face is a recessed black oval with exactly two white vertical capsule eyes and no mouth. Transparent background: no scenery, floor, shadow, text, or effects. Character: {the character the user described}. Pose: standing idle, facing front.

Keep the face and the look unless the user asked for something else (their own style, a real pet's face); then describe what they asked for instead, still one full-body character on a transparent background with two clearly visible open eyes.

From a picture the user sent, their message names where it's saved. Generate the resting pose with that path in `references` and the prompt above, the character described as "the character in the reference picture, redrawn as this mascot" (or "kept as it is", if the user wants their picture itself; then leave out the style sentence). Only if the user wants their exact picture and it is a PNG with a transparent or plain background, use it unchanged: copy it to `DIR/idle.png` instead of generating.

## 3. The three mood poses

Make all three in one `generate_images` call (they're made at the same time), each with `references: ["DIR/idle.png"]` so the character stays the same, and this prompt with the pose filled in:

> The same character as the reference image: identical design, colours, proportions and face. One character, full body, nothing cropped, transparent background: no scenery, floor, shadow, text, Z marks, hearts or other effects. Pose: {pose}.

- `DIR/working.png`: sitting and typing on a small laptop.
- `DIR/waving.png`: sitting and waving one paw (hand, wing or flipper, whatever the character has).
- `DIR/sleeping.png`: curled up asleep with both eyes closed as two small curved lines.

The app draws its own badges, sleepy "z"s and effects around the pet, so the pictures must not.

## 4. Check

Call `save_pet` with the name, the four paths and `check_only: true`. It prints one line per pose and writes a contact sheet; look at the sheet (read the image file it names) to see that the four poses are one character, whole, cut out cleanly.

- `warning: <pose>: no eyes found` for idle, working or waving: remake that pose once, asking for "two clearly visible open eyes", and check again. If it still finds none, carry on; that pose just won't blink.
- An error that a figure is too small or a picture is empty: remake that pose once with "large, filling most of the picture".
- A pose that's clearly a different character, or cropped: remake it once.
- Sleeping never has eyes; that is expected.

## 5. Save

Call `save_pet` again with the same name and paths, without `check_only`. It saves the pet among the user's characters and makes it the desktop pet right away; pass `make_it_the_pet: false` only when the user asked just to add one, not to switch to it.

Then tell the user their new pet is on the desktop and what it looks like. Mention they can pick another pet, or this one again, in Settings › Desktop pet, and use it as the app icon under Characters on App icon. Don't paste file paths or JSON.
