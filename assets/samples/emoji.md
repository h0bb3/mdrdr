# Emoji test 🎉

A tour of the ranges the bundled OpenMoji font covers, so we can
eyeball missing glyphs and tweak `font::is_emoji` as we go.

## Emoticons

😀 😃 😄 😁 😆 😅 🤣 😂 🙂 🙃 😉 😊 😇
🥰 😍 🤩 😘 😗 😚 😙 🥲 😋 😛 😜 🤪 😝
🤑 🤗 🤭 🤫 🤔 🤐 🤨 😐 😑 😶 😏 😒 🙄
😬 🤥 😌 😔 😪 🤤 😴

## Gestures and people

👋 🤚 🖐 ✋ 🖖 👌 🤌 🤏 ✌ 🤞 🫰 🤟 🤘 🤙
👈 👉 👆 🖕 👇 ☝ 👍 👎 ✊ 👊 🤛 🤜 👏 🙌
🫶 👐 🤲 🤝 🙏 ✍

## Nature and food

🌞 ⭐ 🌟 ✨ 🌈 ☀ ⛅ ☁ ⛈ 🌧 ❄ ☃ ⛄ 🌊
🍎 🍐 🍊 🍋 🍌 🍉 🍇 🍓 🍒 🥝 🍑 🍍 🥥
☕ 🍵 🍺 🍷 🍕 🍔 🍟 🍣 🍝 🍩 🍰

## Objects and travel

📝 📚 📖 📕 📘 📗 📙 🔖 💡 🔦 🕯 ✏ 🖊 🖋
🚀 ✈ 🚂 🚗 🚕 🚙 🚌 🚎 🚓 🚑 🚒 🚜 🏎 🛴
🏠 🏢 🏫 🏨 🗼 🗽 🏝 ⛺

## Symbols

❤ 💔 💕 💖 💘 💝 💌 ⭐ ✅ ❌ ⚠ ♻ 🔒 🔑
✔ ✖ ➕ ➖ ➗ ♥ ♦ ♣ ♠ ♪ ♫ ⚡ ☑ ✨

## In context

Mixed inline text: "Hey there 👋, hope the coffee ☕ is still warm.
Ship it 🚀 and we'll celebrate later 🎉."

A list:

- apples 🍎 — the ordinary sort
- pears 🍐 — slightly bruised
- rockets 🚀 — surplus from last launch

> Blockquotes work too 💬 — this is the one above the primitive layer.

```rust
// even in code blocks, though monospace + emoji alignment is loose
fn main() {
    println!("hello 👋"); // 🚀
}
```

## Status check

If you see outline-only pictographs the font is working. If you see
tofu boxes (□), extend `font::is_emoji` to cover the missing range.
