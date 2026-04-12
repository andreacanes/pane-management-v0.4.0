import { JSX, splitProps } from "solid-js";

export interface SkeletonProps extends JSX.HTMLAttributes<HTMLDivElement> {
  width?: string;
  height?: string;
  radius?: string;
}

/** Animated placeholder with a subtle shimmer. */
export function Skeleton(props: SkeletonProps) {
  const [local, rest] = splitProps(props, ["width", "height", "radius", "class", "style"]);
  return (
    <div
      {...rest}
      class={`ui-skeleton${local.class ? " " + local.class : ""}`}
      style={{
        width: local.width ?? "100%",
        height: local.height ?? "14px",
        "border-radius": local.radius ?? "4px",
        ...((local.style as Record<string, string>) ?? {}),
      }}
    />
  );
}

/** Skeleton project card — shows while the initial project list is loading. */
export function SkeletonProjectCard() {
  return (
    <div class="ui-skeleton-card">
      <Skeleton width="60%" height="14px" />
      <Skeleton width="40%" height="10px" />
      <div style={{ display: "flex", gap: "6px", "margin-top": "6px" }}>
        <Skeleton width="50px" height="18px" radius="4px" />
        <Skeleton width="36px" height="18px" radius="4px" />
      </div>
    </div>
  );
}
