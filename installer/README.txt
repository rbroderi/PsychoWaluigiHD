Psycho Waluigi HD
=================

Plays Psycho Waluigi with HD textures, redrawn at 9x the original resolution.
Start it from the "Psycho Waluigi HD" shortcut (Start menu or desktop).

Keys
  F6           switch the look: xBRZ art, cleanEdge art, original pixels
  Alt+Enter    full screen (Esc leaves it)
  F8           show or hide the menu bar
  F9           save screenshots (to hd_runtime\shots)
  Controls menu: rebind keys and set up an XInput gamepad (saved in hd_runtime\controls.cfg)
  Everything else goes to the game: arrows, Shift, Ctrl, Backspace, F2 restart, Enter pause.

What's in this folder
  psychowaluigi.exe          your copy of the game (the game reads its data from it)
  walusave*                  your saves (copied from the original game's folder if they were there)
  hd_runtime\                the game's own runtime, unpacked from psychowaluigi.exe, plus
                             mmfs2.dll (the HD bridge) and hd_view.exe (the HD viewer)
  sprites\                   the game's original images
  sprites_xbrz9x_direct\     HD textures, xBRZ
  sprites_cleanedge9x\       HD textures, cleanEdge
  licenses\                  licences of the upscaling algorithms

Nothing from the game is included in the installer: everything above that comes from the game
was made from the psychowaluigi.exe you picked during setup. To rebuild it (for example after
replacing psychowaluigi.exe), run the installer again.

Uninstalling removes all of it except your saves and controls.cfg.
