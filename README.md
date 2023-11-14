**⚠️ WIP ⚠️**

# Shade

A rust library that helps you modify your X Wallpaper! 

## What does it do ?

Shade takes care of connecting with X11, allocating and updating the pixmaps and setting the righ XAtoms.

This way, you can focus on implementing the logic, whether you'd like to just make a wallpaper setter, 
display some cool numbers like conky does, or make an animated wallpaper. 

Note that to stay as minimal as possible, all drawing logic is left to you. However, for 
simple projects / designs, a feature providing a certain set of drawing algorithms is planned.

## Example

```rust
// Pending review by the O5 Council
todo!()
```

## Documentation

Will be uploaded to docs.rs once the project is ready

## TODO

- Support machines without a compositor 
- Support machines with multiple monitors

## Inspiration and Acknowledgements
- [conky](https://github.com/brndnmtthws/conky/) for the original idea
- [feh](https://github.com/derf/feh) for a huge amount of guidance and serving as a reference
- [Rust XCB Bindings](https://github.com/rtbo/rust-xcb) for providing the framework shade builds on

## Bugs / Contributing
Due to the nature of this project, there should _theoretically_ be only a small room for platform specific issues, as your Xorg Server
(and compositor, if you're running one) should take care of most of the work. 

However, due to my local constraints and the very possible chance of oversights happening, it is possible for all forms of weirdness to
sneak into this code. In that case, please feel free to suggets changes in a pull request or open an issue and I will implement them.
