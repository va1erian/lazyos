//! Triangle meshes and the four shapes the About window shows. Every shape
//! is centred on the origin inside a unit-ish sphere so one camera suits all.

use std::collections::{HashMap, HashSet};
use std::f32::consts::TAU;

use crate::math::Vec3;

/// Indexed triangles with a normal per vertex (shared for a smooth surface,
/// split for a faceted one).
#[derive(Clone, Debug, Default)]
pub struct Mesh {
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub triangles: Vec<[u32; 3]>,
}

/// Which shape is on show.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Knot,
    Torus,
    Crystal,
    Sphere,
}

impl Shape {
    pub const ALL: [Shape; 4] = [Shape::Knot, Shape::Torus, Shape::Crystal, Shape::Sphere];

    pub fn name(self) -> &'static str {
        match self {
            Shape::Knot => "Torus knot (2, 3)",
            Shape::Torus => "Torus",
            Shape::Crystal => "Icosahedron",
            Shape::Sphere => "Icosphere",
        }
    }

    pub fn next(self) -> Shape {
        let at = Shape::ALL.iter().position(|s| *s == self).unwrap_or(0);
        Shape::ALL[(at + 1) % Shape::ALL.len()]
    }

    pub fn build(self) -> Mesh {
        match self {
            Shape::Knot => torus_knot(2, 3, 192, 18, 0.34),
            Shape::Torus => torus(1.0, 0.42, 56, 28),
            Shape::Crystal => icosahedron_flat(1.45),
            Shape::Sphere => icosphere(3, 1.35),
        }
    }
}

impl Mesh {
    /// Each undirected edge once, for the wireframe.
    pub fn edges(&self) -> Vec<[u32; 2]> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for t in &self.triangles {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                if seen.insert((a.min(b), a.max(b))) {
                    out.push([a, b]);
                }
            }
        }
        out
    }
}

/// A closed tube along the (p, q) torus knot, `along` rings of `around`
/// vertices. The frame is built from the knot's own tangent and curvature
/// direction, so the tube never twists.
pub fn torus_knot(p: u32, q: u32, along: u32, around: u32, tube: f32) -> Mesh {
    let point = |u: f32| {
        let r = (q as f32 * u).cos() + 2.0;
        Vec3::new(
            r * (p as f32 * u).cos(),
            r * (p as f32 * u).sin(),
            -(q as f32 * u).sin(),
        ) * 0.45
    };
    let mut mesh = Mesh::default();
    let du = TAU / along as f32 * 0.25;
    for i in 0..along {
        let u = TAU * i as f32 / along as f32;
        let (p1, p2) = (point(u), point(u + du));
        let t = p2 - p1;
        let b = t.cross(p2 + p1).normalized();
        let n = b.cross(t).normalized();
        for j in 0..around {
            let v = TAU * j as f32 / around as f32;
            let dir = n * v.cos() + b * v.sin();
            mesh.positions.push(p1 + dir * tube);
            mesh.normals.push(dir);
        }
    }
    ring_grid(&mut mesh, along, around);
    mesh
}

/// A doughnut of centre radius `big` and tube radius `small`.
pub fn torus(big: f32, small: f32, along: u32, around: u32) -> Mesh {
    let mut mesh = Mesh::default();
    for i in 0..along {
        let (su, cu) = (TAU * i as f32 / along as f32).sin_cos();
        for j in 0..around {
            let (sv, cv) = (TAU * j as f32 / around as f32).sin_cos();
            let dir = Vec3::new(cu * cv, su * cv, sv);
            mesh.positions
                .push(Vec3::new(cu * big, su * big, 0.0) + dir * small);
            mesh.normals.push(dir);
        }
    }
    ring_grid(&mut mesh, along, around);
    mesh
}

/// Quads between consecutive rings of an `along` x `around` grid that wraps
/// in both directions, as two triangles each.
fn ring_grid(mesh: &mut Mesh, along: u32, around: u32) {
    let at = |i: u32, j: u32| (i % along) * around + j % around;
    for i in 0..along {
        for j in 0..around {
            let (a, b, c, d) = (at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1));
            mesh.triangles.push([a, b, c]);
            mesh.triangles.push([a, c, d]);
        }
    }
}

fn icosahedron_vertices() -> ([Vec3; 12], [[u32; 3]; 20]) {
    let t = (1.0 + 5.0_f32.sqrt()) / 2.0;
    let raw = [
        (-1.0, t, 0.0),
        (1.0, t, 0.0),
        (-1.0, -t, 0.0),
        (1.0, -t, 0.0),
        (0.0, -1.0, t),
        (0.0, 1.0, t),
        (0.0, -1.0, -t),
        (0.0, 1.0, -t),
        (t, 0.0, -1.0),
        (t, 0.0, 1.0),
        (-t, 0.0, -1.0),
        (-t, 0.0, 1.0),
    ];
    let v = raw.map(|(x, y, z)| Vec3::new(x, y, z).normalized());
    let f = [
        [0, 11, 5],
        [0, 5, 1],
        [0, 1, 7],
        [0, 7, 10],
        [0, 10, 11],
        [1, 5, 9],
        [5, 11, 4],
        [11, 10, 2],
        [10, 7, 6],
        [7, 1, 8],
        [3, 9, 4],
        [3, 4, 2],
        [3, 2, 6],
        [3, 6, 8],
        [3, 8, 9],
        [4, 9, 5],
        [2, 4, 11],
        [6, 2, 10],
        [8, 6, 7],
        [9, 8, 1],
    ];
    (v, f)
}

/// A faceted icosahedron: every face carries its own flat normal.
pub fn icosahedron_flat(radius: f32) -> Mesh {
    let (v, faces) = icosahedron_vertices();
    let mut mesh = Mesh::default();
    for f in faces {
        let p = f.map(|i| v[i as usize] * radius);
        let n = (p[1] - p[0]).cross(p[2] - p[0]).normalized();
        let base = mesh.positions.len() as u32;
        for corner in p {
            mesh.positions.push(corner);
            mesh.normals.push(n);
        }
        mesh.triangles.push([base, base + 1, base + 2]);
    }
    mesh
}

/// An icosahedron subdivided `levels` times and pushed onto a sphere.
pub fn icosphere(levels: u32, radius: f32) -> Mesh {
    let (v, faces) = icosahedron_vertices();
    let mut positions: Vec<Vec3> = v.to_vec();
    let mut triangles: Vec<[u32; 3]> = faces.to_vec();
    for _ in 0..levels {
        let mut midpoints: HashMap<(u32, u32), u32> = HashMap::new();
        let mut mid = |a: u32, b: u32, positions: &mut Vec<Vec3>| {
            *midpoints.entry((a.min(b), a.max(b))).or_insert_with(|| {
                let m = ((positions[a as usize] + positions[b as usize]) * 0.5).normalized();
                positions.push(m);
                positions.len() as u32 - 1
            })
        };
        let mut next = Vec::with_capacity(triangles.len() * 4);
        for [a, b, c] in triangles {
            let ab = mid(a, b, &mut positions);
            let bc = mid(b, c, &mut positions);
            let ca = mid(c, a, &mut positions);
            next.extend([[a, ab, ca], [b, bc, ab], [c, ca, bc], [ab, bc, ca]]);
        }
        triangles = next;
    }
    Mesh {
        normals: positions.clone(),
        positions: positions.into_iter().map(|p| p * radius).collect(),
        triangles,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_shape_is_well_formed_and_fits_the_camera() {
        for shape in Shape::ALL {
            let mesh = shape.build();
            assert_eq!(mesh.positions.len(), mesh.normals.len(), "{shape:?}");
            assert!(!mesh.triangles.is_empty());
            let n = mesh.positions.len() as u32;
            assert!(mesh.triangles.iter().flatten().all(|&i| i < n));
            let far = mesh
                .positions
                .iter()
                .map(|p| p.length())
                .fold(0.0, f32::max);
            assert!(far < 1.8, "{shape:?} reaches {far}");
            assert!(mesh.normals.iter().all(|n| (n.length() - 1.0).abs() < 1e-3));
        }
    }

    #[test]
    fn icosphere_subdivision_quadruples_faces() {
        assert_eq!(icosphere(0, 1.0).triangles.len(), 20);
        assert_eq!(icosphere(2, 1.0).triangles.len(), 320);
    }

    #[test]
    fn a_closed_surface_has_three_halves_edges_per_face() {
        let m = icosphere(1, 1.0);
        assert_eq!(m.edges().len(), m.triangles.len() * 3 / 2);
    }
}
