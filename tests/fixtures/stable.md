> **Outdated.** Horizon now ships as the **Nightly** release (built from main every few hours): get it through the launcher at https://playhorizon.dev or from the [Nightly release](https://github.com/horizon-rust/horizon/releases/tag/nightly). GitHub keeps calling this one "Latest" only because Nightly is a pre-release.

<h3>Want to play? Download the <a href="https://playhorizon.dev">Horizon launcher</a>, not the files below</h3><p>It installs this version and keeps it up to date. It is also on GitHub: <a href="https://github.com/horizon-rust/horizon/releases?q=launcher&amp;expanded=true">the launcher releases</a>. You need your own copy of Minecraft: Java Edition. The files below are what the launcher installs, and the dedicated server for people who host one.</p>

### Fixed
- item pickup: a stack in the off hand fills before the inventory, as getSlotWithRemainingSpace does (#3067, fixes #2865)
- death message: a creeper's blast names the creeper, not the last mob that hit (#3068, fixes #2978)
- riding: the crosshair skips the mob being ridden, so it can't be hit (#3069, fixes #2870)
- boat: the view stops at 105 degrees at every turn, so it doesn't jitter (#3070, fixes #2983)
- HUD paint: build the sprite key in one reused buffer (#3083)
- Inventory draw: a slot is drawn from a reference, not a copy (#3086)
- Creative screen: draw the selected tab from a reference (#3092)
- Server list: build each row's sprite key once (#3095)
- Effect panels: borrow the name and reuse the icon key (#3097)
- Name tags: copy a custom name only inside the tag's range (#3100)
- Crowded mobs take cramming damage, as vanilla (#3124)
- Shulker bullets are drawn as the spinning bullet model, not a flat sprite (#3127, fixes #3115)
- fix(windows): open URLs with the registered protocol handler (#3210)
- Chat and book links: a show_dialog click event opens its dialog (#3238, fixes #3204)
- Pin the turtle's walk animation to vanilla's rate (#2748, fixes #1776)
- The spawn egg's offspring is a named, tested rule (#2791, fixes #2269)
- Signs: keep laid-out sign text between frames (3458 signs: 124 → 222 fps) (#3284, fixes #1461)
- Prepare: accept Mojang's fifth 26.3 republish (5df1795e, index 917ff6d7) (#3318)

### Known issues
- Shader packs have been removed for now. They will come back later, written again from scratch. Packs in your `shaderpacks` folder are left where they are.

Thanks to @Domanffe, @PH4N7OMx, @degepie, @iLostName, @kacpherek, @levzzz5154, @masa-masa-0314.

Not an official Minecraft product. Not approved by or associated with Mojang or Microsoft.


