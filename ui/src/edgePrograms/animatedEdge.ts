/**
 * Custom Sigma edge program (#25): a flowing dashed-line that animates in
 * the source → target direction, with per-edge intensity controlling dash
 * brightness and flow speed.
 *
 * Closely modelled on Sigma's built-in `EdgeRectangleProgram` — same quad
 * geometry, same anti-aliasing approach across the line normal — with two
 * additions:
 *
 *   1. A `a_intensity` attribute carrying a 0..1 throughput hint per edge.
 *   2. A `u_time` uniform updated each frame, driving the dash phase.
 *
 * Idle edges (intensity ≈ 0) sit as a faint static line, fast edges
 * (intensity ≈ 1) pulse brightly and march quickly. We deliberately keep
 * even quiet edges visible at low alpha so a partial map still shows the
 * shape of the graph — animation is "icing", not "presence".
 *
 * Per-direction handling: the graph model already stores each direction as
 * its own edge (A→B and B→A are distinct). Sigma sees those as two parallel
 * edges. They overlap visually today; splaying / two-line rendering is
 * deferred to a follow-up — this PR delivers the motion, not the layout.
 */
import { EdgeProgram, type ProgramInfo } from "sigma/rendering";
import type { Attributes } from "graphology-types";
import type { EdgeDisplayData, NodeDisplayData, RenderParams } from "sigma/types";

const UNIFORMS = [
  "u_matrix",
  "u_zoomRatio",
  "u_sizeRatio",
  "u_correctionRatio",
  "u_pixelRatio",
  "u_feather",
  "u_minEdgeThickness",
  "u_time",
] as const;

const VERTEX_SHADER_SOURCE = /* glsl */ `
attribute vec4 a_color;
attribute vec2 a_normal;
attribute float a_normalCoef;
attribute vec2 a_positionStart;
attribute vec2 a_positionEnd;
attribute float a_positionCoef;
attribute float a_intensity;

uniform mat3 u_matrix;
uniform float u_sizeRatio;
uniform float u_zoomRatio;
uniform float u_pixelRatio;
uniform float u_correctionRatio;
uniform float u_minEdgeThickness;
uniform float u_feather;
uniform float u_time;

varying vec4 v_color;
varying vec2 v_normal;
varying float v_thickness;
varying float v_feather;
varying float v_t;
varying float v_intensity;
varying float v_phase;

const float bias = 255.0 / 254.0;

void main() {
  float minThickness = u_minEdgeThickness;
  vec2 normal = a_normal * a_normalCoef;
  vec2 position = a_positionStart * (1.0 - a_positionCoef) + a_positionEnd * a_positionCoef;

  float normalLength = length(normal);
  vec2 unitNormal = normal / normalLength;

  // Edge thickness in screen pixels, with a floor so faint edges remain
  // pickable. Same construction as EdgeRectangleProgram.
  float pixelsThickness = max(normalLength, minThickness * u_sizeRatio);
  float webGLThickness = pixelsThickness * u_correctionRatio / u_sizeRatio;

  gl_Position = vec4((u_matrix * vec3(position + unitNormal * webGLThickness, 1)).xy, 0, 1);

  v_thickness = webGLThickness / u_zoomRatio;
  v_normal = unitNormal;
  v_feather = u_feather * u_correctionRatio / u_zoomRatio / u_pixelRatio * 2.0;
  v_color = a_color;
  v_color.a *= bias;

  // 0 at source, 1 at target — drives the dash position along the edge.
  v_t = a_positionCoef;
  v_intensity = a_intensity;
  // Speed of the dash pattern: idle edges drift slowly, busy edges march.
  // Numbers chosen empirically so a saturated edge does ~one full cycle
  // per second at default dash density.
  float speed = 0.18 + a_intensity * 0.55;
  v_phase = u_time * speed;
}
`;

const FRAGMENT_SHADER_SOURCE = /* glsl */ `
precision mediump float;

varying vec4 v_color;
varying vec2 v_normal;
varying float v_thickness;
varying float v_feather;
varying float v_t;
varying float v_intensity;
varying float v_phase;

const float DASH_PERIOD = 0.18; // dashes per unit-edge-length
const float DASH_DUTY = 0.55;   // fraction of the period that is "lit"

void main() {
  // Cross-edge anti-aliasing — interpolated normal length goes 0 at the
  // line centre to 1 at the line edge, just as in EdgeRectangleProgram.
  float dist = length(v_normal) * v_thickness;
  float edgeFalloff = 1.0 - smoothstep(v_thickness - v_feather, v_thickness, dist);

  // Along-edge dash pattern, marching from source toward target as
  // v_phase grows over time. Subtracting v_phase makes positive time
  // shift the dashes *forward* (toward the target).
  float pos = v_t - v_phase;
  float phase = fract(pos / DASH_PERIOD);

  // Soft head + tail give each dash a comet-like profile rather than a
  // hard rectangle. Keeps the animation calm at scale.
  float fade = 0.08;
  float head = smoothstep(0.0, fade, phase);
  float tail = 1.0 - smoothstep(DASH_DUTY - fade, DASH_DUTY, phase);
  float lit = max(0.0, head * tail);

  // Even unlit pixels keep a faint baseline so the static line of the
  // edge is always visible — animation rides on top of presence.
  float along = mix(0.35, 1.0, lit);

  // Idle edges sit dim, saturated edges flare. The intensity squash keeps
  // mid-traffic edges interesting rather than clumping toward dark or bright.
  float brightness = mix(0.55, 1.15, sqrt(v_intensity));

  vec3 rgb = v_color.rgb * along * brightness;
  // Bright pixels can blow past 1.0 when brightness exceeds 1; clamp so
  // the saturation reads as glow rather than colour distortion.
  rgb = min(rgb, vec3(1.0));

  gl_FragColor = vec4(rgb, v_color.a * edgeFalloff);
}
`;

const { UNSIGNED_BYTE, FLOAT } = WebGLRenderingContext;

export default class AnimatedEdgeProgram<
  N extends Attributes = Attributes,
  E extends Attributes = Attributes,
  G extends Attributes = Attributes,
> extends EdgeProgram<(typeof UNIFORMS)[number], N, E, G> {
  /** Wall-clock at program construction. Subtracted from `Date.now()` so
   * `u_time` stays at small magnitudes — avoids float precision drift in
   * the shader over long sessions. */
  private readonly origin = performance.now();

  getDefinition() {
    return {
      VERTICES: 6,
      VERTEX_SHADER_SOURCE,
      FRAGMENT_SHADER_SOURCE,
      METHOD: WebGLRenderingContext.TRIANGLES,
      UNIFORMS,
      ATTRIBUTES: [
        { name: "a_positionStart", size: 2, type: FLOAT },
        { name: "a_positionEnd", size: 2, type: FLOAT },
        { name: "a_normal", size: 2, type: FLOAT },
        { name: "a_color", size: 4, type: UNSIGNED_BYTE, normalized: true },
        { name: "a_intensity", size: 1, type: FLOAT },
      ],
      CONSTANT_ATTRIBUTES: [
        { name: "a_positionCoef", size: 1, type: FLOAT },
        { name: "a_normalCoef", size: 1, type: FLOAT },
      ],
      // Same quad as EdgeRectangleProgram: two triangles, six vertices,
      // pairs of (positionCoef, normalCoef) marking each corner.
      CONSTANT_DATA: [
        [0, 1],
        [0, -1],
        [1, 1],
        [1, 1],
        [0, -1],
        [1, -1],
      ],
    };
  }

  processVisibleItem(
    _edgeIndex: number,
    startIndex: number,
    sourceData: NodeDisplayData,
    targetData: NodeDisplayData,
    data: EdgeDisplayData & { intensity?: number },
  ): void {
    const thickness = data.size || 1;
    const x1 = sourceData.x;
    const y1 = sourceData.y;
    const x2 = targetData.x;
    const y2 = targetData.y;
    const color = floatColor(data.color);

    let dx = x2 - x1;
    let dy = y2 - y1;
    let len = dx * dx + dy * dy;
    let n1 = 0;
    let n2 = 0;
    if (len) {
      len = 1 / Math.sqrt(len);
      n1 = -dy * len * thickness;
      n2 = dx * len * thickness;
    }
    // Defensive clamp on intensity in case a reducer feeds us garbage.
    const intensity = Math.max(0, Math.min(1, data.intensity ?? 0));

    const array = this.array;
    array[startIndex++] = x1;
    array[startIndex++] = y1;
    array[startIndex++] = x2;
    array[startIndex++] = y2;
    array[startIndex++] = n1;
    array[startIndex++] = n2;
    array[startIndex++] = color;
    array[startIndex++] = intensity;
  }

  setUniforms(params: RenderParams, programInfo: ProgramInfo): void {
    const { gl, uniformLocations } = programInfo;
    const {
      u_matrix,
      u_zoomRatio,
      u_feather,
      u_pixelRatio,
      u_correctionRatio,
      u_sizeRatio,
      u_minEdgeThickness,
      u_time,
    } = uniformLocations as Record<(typeof UNIFORMS)[number], WebGLUniformLocation>;

    gl.uniformMatrix3fv(u_matrix, false, params.matrix);
    gl.uniform1f(u_zoomRatio, params.zoomRatio);
    gl.uniform1f(u_sizeRatio, params.sizeRatio);
    gl.uniform1f(u_correctionRatio, params.correctionRatio);
    gl.uniform1f(u_pixelRatio, params.pixelRatio);
    gl.uniform1f(u_feather, params.antiAliasingFeather);
    gl.uniform1f(u_minEdgeThickness, params.minEdgeThickness);
    gl.uniform1f(u_time, (performance.now() - this.origin) / 1000);
  }
}

/**
 * Pack a CSS-style colour string into the same 32-bit float-as-RGBA8
 * representation Sigma's stock programs use. Re-implemented here rather
 * than imported because the helper lives behind a private module path
 * in Sigma's build output — copy is one short function, the indirection
 * isn't worth the brittleness.
 */
const floatColorCache = new Map<string, number>();
const colorCanvas = (() => {
  if (typeof document === "undefined") return null;
  const c = document.createElement("canvas");
  c.width = 1;
  c.height = 1;
  return c;
})();

function floatColor(color: string): number {
  const cached = floatColorCache.get(color);
  if (cached !== undefined) return cached;

  let r = 0,
    g = 0,
    b = 0,
    a = 255;

  // Fast paths for the common formats we emit from the reducer
  // (#rrggbb, rgba(r,g,b,a)). Fall back to the canvas-resolve path for
  // anything else (named CSS colours, hsl(), etc.).
  if (color.charCodeAt(0) === 35 /* '#' */ && (color.length === 7 || color.length === 4)) {
    const hex = color.length === 4
      ? color[1] + color[1] + color[2] + color[2] + color[3] + color[3]
      : color.slice(1);
    r = parseInt(hex.slice(0, 2), 16);
    g = parseInt(hex.slice(2, 4), 16);
    b = parseInt(hex.slice(4, 6), 16);
  } else if (color.startsWith("rgba") || color.startsWith("rgb")) {
    const m = color.match(/[\d.]+/g);
    if (m && m.length >= 3) {
      r = Math.round(parseFloat(m[0]));
      g = Math.round(parseFloat(m[1]));
      b = Math.round(parseFloat(m[2]));
      if (m.length >= 4) a = Math.round(parseFloat(m[3]) * 255);
    }
  } else if (colorCanvas) {
    // Fallback: paint the colour on a 1×1 canvas and read it back.
    const ctx = colorCanvas.getContext("2d");
    if (ctx) {
      ctx.fillStyle = "#000";
      ctx.fillRect(0, 0, 1, 1);
      ctx.fillStyle = color;
      ctx.fillRect(0, 0, 1, 1);
      const px = ctx.getImageData(0, 0, 1, 1).data;
      r = px[0];
      g = px[1];
      b = px[2];
      a = px[3];
    }
  }

  // Pack as the little-endian byte order WebGL reads with
  // UNSIGNED_BYTE attributes (`normalized: true`).
  const packed = new ArrayBuffer(4);
  const view = new DataView(packed);
  view.setUint8(0, r);
  view.setUint8(1, g);
  view.setUint8(2, b);
  view.setUint8(3, a);
  const f = new Float32Array(packed)[0];
  floatColorCache.set(color, f);
  return f;
}
