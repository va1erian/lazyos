# LazyGolf

Fly over a procedurally generated golf course, drawn the way mid-90s golf
games drew theirs: a 256-colour palette, dithered shading, haze, billboard
trees and palette-cycled water.

Every start builds a new 18-hole course from a seed (links, parkland or
mountain): the land, its streams and ponds, the routing of the holes, the
tees, fairways, greens and bunkers, the trees, and par worked out by playing
each hole with a scratch and a bogey player model. Each course is the best
of a search: many sites are surveyed and several routings tried, and the
one with the most relief, the best views from the tees, holes cleanly apart
and the most variety wins (its score is the *quality* in the top panel).

## Controls

| Keys | Action |
|---|---|
| W A S D, arrows | fly, turn |
| Space or E / C or Q | climb / sink |
| Shift | fly four times faster |
| Drag with the left / right button | look around / pan |
| Mouse wheel | flying speed |
| N / P / Home | next hole, previous hole, the first tee |
| Click the map | fly above that spot |
| T | authentic mode: the view paints itself in, far to near |
| R | resolution: automatic, then 1x to 4x pixels |
| M / H | show or hide the map / this help |
| G | generate a new course |
| B | benchmark: fly every hole and report the frame rate |
| Esc | quit |

The window can be resized and maximized. With *automatic* resolution the
view renders at the window's full size and coarsens only if frames drop below
about 22 per second.
