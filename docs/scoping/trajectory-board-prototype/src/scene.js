/* scene.js: the models. Earth, the Moon, the pads and the Apollo hardware,
 * drawn with three.js through a fixed orthographic camera that matches the
 * 2D chart underneath (see G.project in profile.js).
 *
 * The spacecraft are built here from simple shapes, low-poly to sit with
 * Kenney's Space Kit (CC0), which supplies the ground: craters, rocks, the
 * rover, the astronaut, the tracking dish and the assembly building.
 */
(function (KB) {
  'use strict';

  const DEG = Math.PI / 180;

  function hash32(str) {
    let h = 2166136261;
    for (let i = 0; i < str.length; i++) {
      h ^= str.charCodeAt(i);
      h = Math.imul(h, 16777619);
    }
    return h >>> 0;
  }

  // Lattice value noise, enough to break a sphere into continents and maria.
  function noise3(x, y, z) {
    const xi = Math.floor(x), yi = Math.floor(y), zi = Math.floor(z);
    const xf = x - xi, yf = y - yi, zf = z - zi;
    const h = (a, b, c) => {
      let n = Math.imul(a, 374761393) + Math.imul(b, 668265263) + Math.imul(c, 2147483647);
      n = Math.imul(n ^ (n >>> 13), 1274126177);
      return ((n ^ (n >>> 16)) >>> 0) / 4294967296;
    };
    const s = (v) => v * v * (3 - 2 * v);
    const u = s(xf), v = s(yf), w = s(zf);
    const l = (a, b, k) => a + (b - a) * k;
    return l(
      l(l(h(xi, yi, zi), h(xi + 1, yi, zi), u), l(h(xi, yi + 1, zi), h(xi + 1, yi + 1, zi), u), v),
      l(l(h(xi, yi, zi + 1), h(xi + 1, yi, zi + 1), u), l(h(xi, yi + 1, zi + 1), h(xi + 1, yi + 1, zi + 1), u), v),
      w
    );
  }
  const fbm = (x, y, z) => 0.62 * noise3(x, y, z) + 0.38 * noise3(x * 2.3 + 7, y * 2.3 + 3, z * 2.3 + 11);

  // Kit pieces: [model, angle on the cap (deg), z toward the viewer, scale, yaw (deg)]
  const EARTH_SITE = [
    ['hangar_largeB', 109, -0.5, 0.62, 90],
    ['satelliteDish_large', 30, 0.2, 1.1, 35],
  ];
  const MOON_SITE = [
    ['craterLarge', 166, 1.1, 0.95, 0],
    ['crater', 12, 1.5, 0.85, 0],
    ['rock_largeA', 174, -0.9, 0.75, 0],
    ['rocks_smallA', 8, -0.8, 0.9, 0],
    ['rock_largeB', 18, 2.3, 0.55, 40],
    ['crater', 103, 2.35, 0.7, 0],
    ['rocks_smallB', 62, 2.45, 0.8, 0],
    ['craterLarge', 128, -2.1, 0.8, 0],
    ['rock', 44, -1.9, 0.9, 20],
  ];
  // Company for the landers: [model, angle, z, scale, yaw, landings needed]
  const MOON_EXTRAS = [
    ['astronautA', 89.5, 0.15, 0.95, 10, 1],
    ['rover', 80, 1.75, 1.15, -30, 4],
    ['astronautB', 111, 1.7, 0.95, -20, 6],
  ];
  const KIT_USED = EARTH_SITE.concat(MOON_SITE, MOON_EXTRAS).map((s) => s[0]).filter((v, i, a) => a.indexOf(v) === i);

  function createScene(canvas) {
    const THREE = globalThis.THREE;
    if (!THREE) return null;
    let renderer;
    try {
      renderer = new THREE.WebGLRenderer({ canvas, antialias: true, alpha: true });
    } catch (err) {
      return null;
    }
    renderer.setClearColor(0x000000, 0);

    const scene = new THREE.Scene();
    const cam = new THREE.OrthographicCamera(-1, 1, 1, -1, 1, 400);
    scene.add(new THREE.HemisphereLight(0xffffff, 0x8fa4bd, 0.66));
    const sun = new THREE.DirectionalLight(0xfff3df, 0.78);
    sun.position.set(-0.55, 0.8, 0.85);
    scene.add(sun);

    const P = KB.PROFILE, SCALE = P.SCALE;
    const lib = {}; // kit name -> recentred Group
    const mats = {}, deadMats = {};
    const X = new THREE.Vector3(1, 0, 0), Z = new THREE.Vector3(0, 0, 1);
    const qz = new THREE.Quaternion(), qx = new THREE.Quaternion();
    let G = null;
    let site = null, clouds = null, markers = [], extras = [];
    const crafts = new Map();
    const api = { ready: false, kit: false };

    function mat(name, hex) {
      if (!mats[name]) {
        mats[name] = new THREE.MeshLambertMaterial({ color: hex });
        const c = mats[name].color, g = (c.r + c.g + c.b) / 3;
        deadMats[name] = new THREE.MeshLambertMaterial({ color: new THREE.Color(g * 0.5 + 0.06, g * 0.52 + 0.07, g * 0.56 + 0.09) });
      }
      return name;
    }
    for (const [n, hex] of [['white', 0xf3f4f1], ['black', 0x24272c], ['silver', 0xc5cbd4], ['steel', 0x8f97a3], ['gold', 0xdba640], ['goldDark', 0xb07f28], ['engine', 0x4b505a], ['tower', 0xc9482f], ['pad', 0x8b8f96], ['flag', 0xe0633a], ['chute', 0xf08a2c], ['shield', 0x5a4636]]) mat(n, hex);

    // --- kit -----------------------------------------------------------------
    function loadKit(kit) {
      if (!kit || !THREE.GLTFLoader) return Promise.resolve(false);
      const loader = new THREE.GLTFLoader();
      const jobs = KIT_USED.filter((n) => kit[n]).map(
        (n) =>
          new Promise((resolve) => {
            const bin = atob(kit[n]), buf = new Uint8Array(bin.length);
            for (let i = 0; i < bin.length; i++) buf[i] = bin.charCodeAt(i);
            loader.parse(
              buf.buffer, '',
              (gltf) => {
                const obj = gltf.scene;
                obj.traverse((m) => {
                  if (!m.isMesh) return;
                  const src = Array.isArray(m.material) ? m.material[0] : m.material;
                  m.userData.src = src.name || '_defaultMat';
                  m.material = new THREE.MeshLambertMaterial({ color: src.color ? src.color.clone() : new THREE.Color(0xffffff) });
                });
                obj.updateMatrixWorld(true);
                const box = new THREE.Box3().setFromObject(obj), c = box.getCenter(new THREE.Vector3());
                obj.position.set(-c.x, -box.min.y, -c.z);
                const holder = new THREE.Group();
                holder.add(obj);
                lib[n] = holder;
                resolve(true);
              },
              () => resolve(false)
            );
          })
      );
      return Promise.all(jobs).then((r) => r.length > 0 && r.every(Boolean));
    }

    // --- shapes ----------------------------------------------------------------
    const flat = (geo) => {
      const g = geo.index ? geo.toNonIndexed() : geo;
      g.computeVertexNormals();
      return g;
    };
    function mesh(geo, name) {
      const m = new THREE.Mesh(flat(geo), mats[name]);
      m.userData.mat = name;
      return m;
    }
    // A tube from height y to y + h, radius r0 at the bottom and r1 at the top.
    const tube = (r0, r1, y, h, name, seg) => {
      const g = new THREE.CylinderGeometry(r1, r0, h, seg || 12);
      g.translate(0, y + h / 2, 0);
      return mesh(g, name);
    };
    const group = (...kids) => {
      const g = new THREE.Group();
      for (const k of kids) g.add(k);
      return g;
    };

    function buildStack() {
      const fins = group();
      for (let i = 0; i < 4; i++) {
        const f = mesh(new THREE.BoxGeometry(0.3, 0.42, 0.04), 'white');
        f.position.set(Math.cos((i * Math.PI) / 2) * 0.52, 0.21, Math.sin((i * Math.PI) / 2) * 0.52);
        f.rotation.y = -(i * Math.PI) / 2;
        fins.add(f);
      }
      const s1 = group(tube(0.5, 0.45, 0, 0.2, 'engine'), tube(0.45, 0.45, 0.2, 1.8, 'white'), tube(0.456, 0.456, 0.3, 0.3, 'black'), tube(0.456, 0.456, 1.5, 0.26, 'black'), fins);
      const s2 = group(tube(0.456, 0.456, 2.0, 0.14, 'black'), tube(0.45, 0.45, 2.14, 1.23, 'white'), tube(0.456, 0.456, 3.05, 0.14, 'black'), tube(0.45, 0.3, 3.37, 0.3, 'white'));
      const s3 = group(tube(0.3, 0.3, 3.67, 0.95, 'white'), tube(0.305, 0.305, 3.72, 0.12, 'black'), tube(0.305, 0.305, 4.38, 0.1, 'black'), tube(0.3, 0.2, 4.62, 0.3, 'white'), tube(0.2, 0.2, 4.92, 0.36, 'silver'), tube(0.2, 0.05, 5.28, 0.24, 'steel'));
      const les = group(tube(0.028, 0.028, 5.52, 0.2, 'white', 6), tube(0.06, 0.012, 5.7, 0.1, 'engine', 6));
      return { g: group(s1, s2, s3, les), parts: [s1, s2, s3, les], flameY: [0, 2.0, 3.67], flameS: [1.25, 1.25, 0.8] };
    }

    function buildLM() {
      const legs = group();
      for (let i = 0; i < 4; i++) {
        const a = Math.PI / 4 + (i * Math.PI) / 2, c = Math.cos(a), s = Math.sin(a);
        const strut = mesh(new THREE.CylinderGeometry(0.022, 0.022, 0.42, 5), 'goldDark');
        strut.position.set(c * 0.41, 0.2, s * 0.41);
        strut.rotation.set(0, -a, 0);
        strut.rotateZ(-0.56);
        const foot = tube(0.085, 0.085, 0, 0.03, 'goldDark', 8);
        foot.position.set(c * 0.52, 0, s * 0.52);
        legs.add(strut, foot);
      }
      const stage = tube(0.34, 0.34, 0.3, 0.26, 'gold', 8);
      stage.rotation.y = Math.PI / 8;
      const desc = group(legs, stage, tube(0.13, 0.06, 0.16, 0.14, 'engine', 8));
      const cabin = mesh(new THREE.DodecahedronGeometry(0.27, 0), 'silver');
      cabin.scale.set(1.08, 0.82, 0.92);
      cabin.position.y = 0.77;
      const face = mesh(new THREE.BoxGeometry(0.3, 0.16, 0.06), 'engine');
      face.position.set(0, 0.8, 0.22);
      const dish = tube(0.012, 0.012, 0.9, 0.16, 'steel', 5);
      dish.position.x = 0.13;
      const cup = tube(0.02, 0.075, 1.04, 0.04, 'white', 8);
      cup.position.x = 0.13;
      const asc = group(cabin, face, dish, cup);
      return { g: group(desc, asc), desc, asc };
    }

    function buildCSM() {
      return group(tube(0.15, 0.06, 0, 0.22, 'engine', 10), tube(0.24, 0.24, 0.22, 0.62, 'silver'), tube(0.245, 0.245, 0.3, 0.1, 'white'), tube(0.24, 0.07, 0.84, 0.36, 'steel'));
    }

    function buildCM() {
      const chutes = group();
      for (let i = 0; i < 3; i++) {
        const a = (i * Math.PI * 2) / 3 + 0.5;
        const g = new THREE.SphereGeometry(0.3, 8, 4, 0, Math.PI * 2, 0, Math.PI / 2);
        const c = mesh(g, i === 1 ? 'white' : 'chute');
        c.position.set(Math.cos(a) * 0.3, 1.25, Math.sin(a) * 0.3);
        c.rotation.set(Math.sin(a) * 0.35, 0, -Math.cos(a) * 0.35);
        chutes.add(c);
      }
      return { g: group(tube(0.3, 0.3, 0, 0.05, 'shield'), tube(0.3, 0.07, 0.05, 0.4, 'steel'), chutes), chutes };
    }

    const flameGeo = new THREE.ConeGeometry(0.36, 1.7, 8), coreGeo = new THREE.ConeGeometry(0.19, 1.1, 8);
    flameGeo.rotateX(Math.PI);
    flameGeo.translate(0, -0.85, 0);
    coreGeo.rotateX(Math.PI);
    coreGeo.translate(0, -0.55, 0);
    const flameMat = new THREE.MeshBasicMaterial({ color: 0xffb347, transparent: true, opacity: 0.92 });
    const coreMat = new THREE.MeshBasicMaterial({ color: 0xfff4cf });

    function buildCraft(id) {
      const root = new THREE.Group(), spin = new THREE.Group();
      root.add(spin);
      const stack = buildStack(), lm = buildLM(), cm = buildCM();
      const docked = buildLM();
      docked.g.rotation.z = Math.PI;
      docked.g.position.y = 2.2;
      const csmlm = group(buildCSM(), docked.g);
      const flame = group(new THREE.Mesh(flameGeo, flameMat), new THREE.Mesh(coreGeo, coreMat));
      spin.add(stack.g, csmlm, lm.g, cm.g, flame);
      const mate = group(buildCSM());
      mate.children[0].position.y = -0.6;
      mate.visible = false;
      scene.add(root, mate);
      return { root, spin, stack, csmlm, lm, cm, flame, mate, dead: false, seed: (hash32(id) % 628) / 100 };
    }
    function setDead(r, dead) {
      if (r.dead === dead) return;
      r.dead = dead;
      r.root.traverse((m) => {
        if (m.isMesh && m.userData.mat) m.material = (dead ? deadMats : mats)[m.userData.mat];
      });
    }
    function dropCraft(id) {
      const r = crafts.get(id);
      if (!r) return;
      scene.remove(r.root, r.mate);
      crafts.delete(id);
    }

    // --- bodies ----------------------------------------------------------------
    function planet(R, paint) {
      const geo = new THREE.IcosahedronGeometry(R, 6);
      const pos = geo.attributes.position, col = new Float32Array(pos.count * 3), c = new THREE.Color();
      for (let i = 0; i < pos.count; i += 3) {
        const x = (pos.getX(i) + pos.getX(i + 1) + pos.getX(i + 2)) / (3 * R);
        const y = (pos.getY(i) + pos.getY(i + 1) + pos.getY(i + 2)) / (3 * R);
        const z = (pos.getZ(i) + pos.getZ(i + 1) + pos.getZ(i + 2)) / (3 * R);
        paint(c, x, y, z);
        for (let k = 0; k < 3; k++) {
          col[(i + k) * 3] = c.r;
          col[(i + k) * 3 + 1] = c.g;
          col[(i + k) * 3 + 2] = c.b;
        }
      }
      geo.setAttribute('color', new THREE.BufferAttribute(col, 3));
      geo.computeVertexNormals(); // non-indexed, so this gives flat facets
      return new THREE.Mesh(geo, new THREE.MeshLambertMaterial({ vertexColors: true }));
    }
    const lerpHex = (c, a, b, k) => c.setHex(a).lerp(new THREE.Color(b), k < 0 ? 0 : k > 1 ? 1 : k);
    function paintEarth(c, x, y, z) {
      const n = fbm(x * 1.7 + 4.2, y * 1.7 + 1.3, z * 1.7 + 9.1), fine = noise3(x * 6 + 2, y * 6 + 5, z * 6 + 8);
      if (y < -0.87 + fine * 0.05) return c.setHex(0xf1f6f9);
      // Open sea where a returning capsule comes down.
      if (x * Math.cos(G.thS) + y * Math.sin(G.thS) > 0.955 - fine * 0.02 && Math.abs(z) < 0.5) return lerpHex(c, 0x2f79bd, 0x55a7df, (n - 0.2) * 2.4);
      if (y > 0.83) return lerpHex(c, 0xcfc58a, 0xb9c680, fine);
      if (n > 0.535 || y > 0.74) return lerpHex(c, 0x5fae5c, 0x9ccb72, fine * 1.2 - (y > 0.74 ? 0 : 0.1));
      return lerpHex(c, 0x2f79bd, 0x55a7df, (n - 0.2) * 2.4);
    }
    function paintMoon(c, x, y, z) {
      const n = fbm(x * 1.6 + 3.1, y * 1.6 + 8.4, z * 1.6 + 5.7), fine = noise3(x * 8 + 1, y * 8 + 9, z * 8 + 4);
      // Seas of dark basalt low on the near side, bright highlands where the craft land.
      if (n < 0.43 && y < 0.55) return lerpHex(c, 0x7d828c, 0x8f949d, fine);
      return lerpHex(c, 0xb4b7bd, 0xd6d7da, (n - 0.4) * 1.6 + fine * 0.35);
    }

    function orient(obj, n, yaw) {
      const a = Math.atan2(n[1], n[0]) - Math.PI / 2, b = Math.asin(Math.max(-1, Math.min(1, n[2])));
      qz.setFromAxisAngle(Z, a);
      qx.setFromAxisAngle(X, b);
      obj.quaternion.copy(qz).multiply(qx);
      if (yaw) obj.rotateY(yaw);
    }
    function stand(obj, body, th, z, h, yaw) {
      const g = G.surface(body, th, z, h);
      obj.position.set(g.pos[0], g.pos[1], g.pos[2]);
      orient(obj, g.n, yaw || 0);
      return obj;
    }
    const MOON_GREY = { rock: 0xa9acb3, rockDark: 0x858991, rockTrack: 0x9a9da4 };
    function place(body, spec, grey) {
      const src = lib[spec[0]];
      if (!src) return null;
      const o = stand(src.clone(true), body, spec[1] * DEG, spec[2], -0.03, spec[4] * DEG);
      if (grey) o.traverse((m) => {
        if (m.isMesh && MOON_GREY[m.userData.src]) m.material = new THREE.MeshLambertMaterial({ color: MOON_GREY[m.userData.src] });
      });
      o.scale.setScalar(spec[3]);
      o.userData.s = spec[3];
      return o;
    }

    function build() {
      if (site) scene.remove(site);
      site = new THREE.Group();
      scene.add(site);
      markers = [];
      extras = [];
      const E = G.E, M = G.M;

      const earth = planet(E.R, paintEarth);
      earth.position.set(E.x, E.y, 0);
      const moon = planet(M.R, paintMoon);
      moon.position.set(M.x, M.y, 0);
      site.add(earth, moon);

      // Weather: small faceted cloud banks drifting round the middle latitudes.
      clouds = new THREE.Group();
      clouds.position.set(E.x, E.y, 0);
      const puff = new THREE.IcosahedronGeometry(1, 0), cloudMat = new THREE.MeshLambertMaterial({ color: 0xffffff });
      for (let i = 0; i < 8; i++) {
        const lat = (-34 + ((i * 47) % 60)) * DEG, lon = ((i * 137.5) % 360) * DEG, r = E.R + 0.16;
        const bank = new THREE.Group();
        bank.position.set(Math.cos(lat) * Math.sin(lon) * r, Math.sin(lat) * r, Math.cos(lat) * Math.cos(lon) * r);
        bank.lookAt(clouds.position);
        const s = 0.4 + ((i * 7) % 5) * 0.04;
        for (const [dx, dy, k] of [[0, 0, 1], [1.05, 0.1, 0.74], [-1.0, -0.06, 0.66], [0.35, 0.42, 0.56]]) {
          const m = new THREE.Mesh(puff, cloudMat);
          m.position.set(dx * s, dy * s, 0);
          m.scale.set(s * k * 1.15, s * k * 0.62, s * k * 0.4);
          m.rotation.set(i, dx * 3, dy * 5);
          bank.add(m);
        }
        clouds.add(bank);
      }
      site.add(clouds);

      // Pads: a slab and a red umbilical tower beside where the stack stands.
      for (const pad of G.pads) {
        const tower = group(tube(0.11, 0.09, 0, 3.0, 'tower', 4), tube(0.035, 0.035, 3.0, 0.32, 'tower', 4));
        for (const y of [0.9, 1.75, 2.45, 2.9]) {
          const arm = mesh(new THREE.BoxGeometry(0.36, 0.06, 0.06), 'tower');
          arm.position.set(0.2, y, 0);
          tower.add(arm);
        }
        tower.position.x = -0.56;
        site.add(stand(group(tube(0.62, 0.56, -0.02, 0.07, 'pad', 8), tower), E, pad.th, pad.z, 0, 0));
      }
      for (const s of EARTH_SITE) {
        const o = place(E, s);
        if (o) site.add(o);
      }
      for (const s of MOON_SITE) {
        const o = place(M, s, true);
        if (o) site.add(o);
      }
      for (const s of MOON_EXTRAS) {
        const o = place(M, s, true);
        if (o) {
          o.visible = false;
          o.userData.after = s[5];
          site.add(o);
          extras.push(o);
        }
      }

      // One marker per landing site: the descent stage that stays, and a flag.
      const k = SCALE * P.SIZE.lm;
      for (const s of G.sites) {
        const stage = buildLM();
        stage.asc.visible = false;
        const flag = group(tube(0.016, 0.016, 0, 1.15, 'white', 5));
        const cloth = mesh(new THREE.BoxGeometry(0.46, 0.28, 0.02), 'flag');
        cloth.position.set(0.24, 1.0, 0);
        flag.add(cloth);
        flag.position.set(0.72, 0, 0.1);
        const m = stand(group(stage.g, flag), M, s.th, s.z, 0.05, 0);
        m.scale.setScalar(k);
        m.visible = false;
        site.add(m);
        markers.push({ g: m, stage: stage.g, flag });
      }
    }

    // --- per frame ---------------------------------------------------------------
    const easeBack = (u) => {
      if (u <= 0) return 0;
      if (u >= 1) return 1;
      const k = u - 1;
      return 1 + 2.2 * k * k * k + 1.2 * k * k;
    };
    function aim(obj, dx, dy, dz) {
      const a = Math.atan2(dy, dx) - Math.PI / 2, b = Math.asin(Math.max(-1, Math.min(1, dz)));
      qz.setFromAxisAngle(Z, a);
      qx.setFromAxisAngle(X, b);
      obj.quaternion.copy(qz).multiply(qx);
    }

    /* frame.crafts: [{ id, pose }], frame.sites: [{ stage: 0|1, flag: 0..1 } per site],
     * frame.landed: landings so far, frame.wall: seconds. */
    api.frame = function (frame) {
      if (!api.ready) return;
      const seen = new Set();
      for (const c of frame.crafts) {
        const p = c.pose;
        seen.add(c.id);
        let r = crafts.get(c.id);
        if (!r) {
          r = buildCraft(c.id);
          crafts.set(c.id, r);
        }
        setDead(r, p.dead);
        const lmKind = p.kind === 'lm' || p.kind === 'ascent';
        r.root.position.set(p.x, p.y, p.z);
        aim(r.root, p.dx, p.dy, p.dz);
        r.root.scale.setScalar(SCALE * p.size * Math.max(0.001, p.grow));
        r.spin.rotation.y = lmKind || p.kind === 'cm' || p.phase === 'pad' ? r.seed * 0.1 : r.seed + p.roll;
        r.stack.g.visible = p.kind === 'stack';
        r.csmlm.visible = p.kind === 'csmlm';
        r.lm.g.visible = lmKind;
        r.cm.g.visible = p.kind === 'cm';
        let flameY = 0, flameS = 1;
        if (p.kind === 'stack') {
          const parts = r.stack.parts;
          for (let i = 0; i < 4; i++) {
            const s = easeBack(p.stack * 4 - i), gone = (i === 0 && p.stage >= 1) || ((i === 1 || i === 3) && p.stage >= 2);
            parts[i].visible = s > 0.01 && !gone;
            parts[i].scale.set(1, Math.max(0.01, s), 1);
            parts[i].position.y = i === 0 ? 0 : [0, 2.0, 3.67, 5.52][i] * (1 - Math.max(0.01, s));
          }
          r.stack.g.position.y = -p.mid;
          flameY = r.stack.flameY[p.stage] - p.mid;
          flameS = r.stack.flameS[p.stage];
        } else if (p.kind === 'csmlm') {
          r.csmlm.position.y = -p.anchor * 1.1;
          flameY = 0.05 - p.anchor * 1.1;
          flameS = 0.5;
        } else if (lmKind) {
          r.lm.desc.visible = p.kind === 'lm';
          r.lm.g.position.y = -p.anchor * 0.5;
          flameY = (p.kind === 'lm' ? 0.2 : 0.56) - p.anchor * 0.5;
          flameS = 0.42;
        } else {
          r.cm.g.position.y = -p.anchor * 0.25;
          r.cm.chutes.visible = p.chute > 0.02;
          r.cm.chutes.scale.setScalar(Math.max(0.01, easeBack(p.chute)));
        }
        if (p.burn > 0.02) {
          const flick = frame.still ? 1 : 0.86 + 0.2 * Math.sin(frame.wall * 31 + r.seed * 9) + 0.08 * Math.sin(frame.wall * 57 + r.seed);
          r.flame.visible = true;
          r.flame.position.y = flameY;
          r.flame.scale.set(flameS * (0.75 + 0.3 * p.burn), flameS * p.burn * flick, flameS * (0.75 + 0.3 * p.burn));
        } else r.flame.visible = false;
        const m = p.mate;
        r.mate.visible = !!m && m.a > 0.02;
        if (r.mate.visible) {
          r.mate.position.set(m.x, m.y, 0);
          aim(r.mate, m.dx, m.dy, 0);
          r.mate.scale.setScalar(SCALE * P.SIZE.flight * m.a);
        }
      }
      for (const id of Array.from(crafts.keys())) if (!seen.has(id)) dropCraft(id);

      for (let i = 0; i < markers.length; i++) {
        const s = frame.sites[i], m = markers[i];
        m.g.visible = !!s && (s.stage > 0 || s.flag > 0.01);
        if (!m.g.visible) continue;
        m.stage.visible = s.stage > 0;
        m.flag.visible = s.flag > 0.01;
        m.flag.scale.set(1, Math.max(0.01, easeBack(s.flag)), 1);
      }
      for (const o of extras) {
        o.visible = frame.landed >= o.userData.after;
      }
      if (clouds && !frame.still) clouds.rotation.y = frame.wall * 0.035;
      renderer.render(scene, cam);
    };

    /* View: w x h CSS pixels, k pixels per world unit, centred on the
     * projected point (cx, cy). Must agree with the chart's transform. */
    api.setView = function (w, h, dpr, k, cx, cy) {
      renderer.setPixelRatio(dpr);
      renderer.setSize(w, h, false);
      cam.left = -w / (2 * k);
      cam.right = w / (2 * k);
      cam.top = h / (2 * k);
      cam.bottom = -h / (2 * k);
      const yw = cy / G.cosE;
      cam.position.set(cx, yw + 200 * G.sinE, 200 * G.cosE);
      cam.up.set(0, 1, 0);
      cam.lookAt(cx, yw, 0);
      cam.updateProjectionMatrix();
    };

    // Where the camera puts a world point, in CSS pixels. The chart must agree.
    api.toPx = function (x, y, z, w, h) {
      const v = new THREE.Vector3(x, y, z).project(cam);
      return [((v.x + 1) / 2) * w, ((1 - v.y) / 2) * h];
    };

    api.setGeom = function (g) {
      G = g;
      if (api.ready) {
        for (const id of Array.from(crafts.keys())) dropCraft(id);
        build();
      }
    };

    api.init = function (g, kit) {
      G = g;
      return loadKit(kit).then((ok) => {
        api.kit = ok;
        build();
        api.ready = true;
        return ok;
      });
    };

    return api;
  }

  KB.createScene = createScene;
  KB.KIT_USED = KIT_USED;
})((globalThis.KB = globalThis.KB || {}));
