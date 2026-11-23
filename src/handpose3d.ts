// 手部关键点 3D 视图：当前帧的手部骨架，随视频播放同步。与 model-qc-worker Web Demo 的 handpose3d.js 相同。
//
// 坐标：MediaPipe 的 world 坐标以手的几何中心为原点（米），不含手在相机中的绝对位置。
// 这里用画面坐标（image，归一化）给出手在画面中的位置，再叠加 world 坐标表现手的 3D 形状：
//   场景 x = (image.x - 0.5) * 宽高比，场景 y = -(image.y - 0.5)，手的形状 = world 坐标 * HAND，围绕手腕摆放。
// 深度方向只反映手内部各点的前后关系，不是手到相机的真实距离。
import * as THREE from "three";
import { OrbitControls } from "three/examples/jsm/controls/OrbitControls.js";
import type { HandposeData, HandposeFrame, Hand } from "./api";

const HAND = 2.2; // world 坐标（米）放大倍数，手约 0.18m → 0.4 场景单位
const SIDE_COLOR: Record<string, number> = { Left: 0x3c9b74, Right: 0xd18b3c };
const TIP = new Set([0, 4, 8, 12, 16, 20]);

interface HandView {
  bones: THREE.LineSegments;
  joints: THREE.InstancedMesh;
}

export class HandPose3D {
  private renderer: THREE.WebGLRenderer;
  private scene = new THREE.Scene();
  private camera = new THREE.PerspectiveCamera(40, 1, 0.01, 50);
  private controls: OrbitControls;
  private frameBox: THREE.LineSegments;
  private hands: Record<string, HandView> = {};
  private data: HandposeData | null = null;
  private times: number[] = [];
  private aspect = 16 / 10;
  private raf = 0;
  private observer: ResizeObserver;

  constructor(private container: HTMLElement) {
    this.renderer = new THREE.WebGLRenderer({ antialias: true });
    this.renderer.setPixelRatio(Math.min(2, window.devicePixelRatio || 1));
    this.renderer.setClearColor(0x151e1a);
    container.append(this.renderer.domElement);

    this.controls = new OrbitControls(this.camera, this.renderer.domElement);
    this.controls.enableDamping = true;
    this.resetView();

    this.scene.add(new THREE.AmbientLight(0xffffff, 0.9));
    const grid = new THREE.GridHelper(2.4, 12, 0x3d5747, 0x26372d);
    grid.position.y = -0.5;
    this.scene.add(grid);
    this.frameBox = new THREE.LineSegments(new THREE.EdgesGeometry(new THREE.PlaneGeometry(1, 1)),
      new THREE.LineBasicMaterial({ color: 0x4f6d5a }));
    this.scene.add(this.frameBox, new THREE.AxesHelper(0.15));

    for (const side of ["Left", "Right"]) this.hands[side] = this.makeHand(SIDE_COLOR[side]);

    this.observer = new ResizeObserver(() => this.resize());
    this.observer.observe(container);
    const loop = () => {
      this.raf = requestAnimationFrame(loop);
      this.controls.update();
      this.renderer.render(this.scene, this.camera);
    };
    loop();
  }

  private makeHand(color: number): HandView {
    const bones = new THREE.LineSegments(new THREE.BufferGeometry(), new THREE.LineBasicMaterial({ color }));
    const joints = new THREE.InstancedMesh(new THREE.SphereGeometry(0.012, 10, 8),
      new THREE.MeshBasicMaterial({ color: 0xffffff }), 21);
    bones.visible = joints.visible = false;
    this.scene.add(bones, joints);
    return { bones, joints };
  }

  load(data: HandposeData) {
    this.data = data;
    this.times = data.frames.map((f) => f.t);
    this.aspect = data.width && data.height ? data.width / data.height : 16 / 10;
    this.frameBox.scale.set(this.aspect, 1, 1);
    const n = data.connections.length;
    for (const view of Object.values(this.hands)) {
      view.bones.geometry.setAttribute("position", new THREE.Float32BufferAttribute(new Float32Array(n * 6), 3));
    }
    this.update(this.times[0] ?? 0);
  }

  /** 二分查找不晚于 t 的最近一帧 */
  indexAt(t: number): number {
    const times = this.times;
    if (!times.length || t < times[0]) return -1;
    let lo = 0, hi = times.length - 1;
    while (lo < hi) {
      const mid = (lo + hi + 1) >> 1;
      if (times[mid] <= t) lo = mid; else hi = mid - 1;
    }
    return lo;
  }

  /** t 时刻对应的关键点帧；间隔超过 0.5 秒（无数据）时返回 null */
  frameAt(t: number): HandposeFrame | null {
    const i = this.indexAt(t);
    return i >= 0 && this.data && t - this.times[i] <= 0.5 ? this.data.frames[i] : null;
  }

  private position(hand: Hand, k: number) {
    const img = hand.image, w = hand.world;
    const ax = (img[0][0] - 0.5) * this.aspect, ay = -(img[0][1] - 0.5);
    return new THREE.Vector3(ax + (w[k][0] - w[0][0]) * HAND, ay - (w[k][1] - w[0][1]) * HAND, -(w[k][2] - w[0][2]) * HAND);
  }

  update(t: number) {
    if (!this.data) return;
    const frame = this.frameAt(t);
    const present = new Set<string>();
    for (const hand of frame ? frame.hands : []) {
      const view = this.hands[hand.side];
      if (!view || present.has(hand.side)) continue;
      present.add(hand.side);
      const points = hand.world.map((_, k) => this.position(hand, k));
      const array = view.bones.geometry.attributes.position.array as Float32Array;
      this.data.connections.forEach(([a, b], n) => {
        points[a].toArray(array, n * 6);
        points[b].toArray(array, n * 6 + 3);
      });
      view.bones.geometry.attributes.position.needsUpdate = true;
      view.bones.geometry.computeBoundingSphere();
      const m = new THREE.Matrix4();
      points.forEach((p, k) => {
        const s = TIP.has(k) ? 1.4 : 1;
        m.makeScale(s, s, s).setPosition(p);
        view.joints.setMatrixAt(k, m);
      });
      view.joints.instanceMatrix.needsUpdate = true;
    }
    for (const [side, view] of Object.entries(this.hands)) {
      view.bones.visible = view.joints.visible = present.has(side);
    }
  }

  resize() {
    const { clientWidth: w, clientHeight: h } = this.container;
    if (!w || !h) return;
    this.renderer.setSize(w, h, false);
    this.camera.aspect = w / h;
    this.camera.updateProjectionMatrix();
  }

  resetView() {
    this.camera.position.set(0.9, 0.5, 2.1);
    this.controls.target.set(0, 0, 0);
  }

  dispose() {
    cancelAnimationFrame(this.raf);
    this.observer.disconnect();
    this.renderer.dispose();
    this.renderer.domElement.remove();
  }
}
