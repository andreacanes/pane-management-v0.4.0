import { JSX, splitProps } from "solid-js";

type Variant = "primary" | "secondary" | "ghost" | "danger";
type Size = "sm" | "md";

export interface ButtonProps extends JSX.ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  size?: Size;
}

export function Button(props: ButtonProps) {
  const [local, rest] = splitProps(props, ["variant", "size", "class", "children"]);
  const variant = () => local.variant ?? "secondary";
  const size = () => local.size ?? "md";
  return (
    <button
      {...rest}
      class={`ui-btn ui-btn-${variant()} ui-btn-${size()}${local.class ? " " + local.class : ""}`}
    >
      {local.children}
    </button>
  );
}
