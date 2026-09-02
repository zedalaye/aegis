/**
 * The "+" on a rail heading (PLAN 7.10 chrome, same place Orca puts one).
 *
 * A sibling of the disclosure, never nested in it — a button inside a button
 * is invalid HTML and would make "add" also fold the section.
 *
 * Two shapes. **One action** is a click: add a project, or add a session when
 * there is only one identity to open as. **A list** is a menu: the identities
 * a new session can be bound to. Creating a session has always been one click
 * in the common case; a menu that opened to a single name would make that
 * case worse to serve the rare one. The menu is therefore only drawn when
 * there is something to choose.
 */

import { useEffect, useId, useRef, useState } from "react";

export type RailAddItem = {
  readonly id: string;
  readonly label: string;
  /** Quieter second line — a role, a hint. Omitted rather than empty. */
  readonly hint?: string;
  readonly onSelect: () => void;
};

export default function RailAdd({
  label,
  disabled = false,
  onClick,
  items,
}: {
  /** Name of the action, on the button as tooltip and `aria-label`. */
  readonly label: string;
  readonly disabled?: boolean;
  /** Taken when there is no menu to open — the one-action case. */
  readonly onClick?: () => void;
  readonly items?: readonly RailAddItem[];
}) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const menuId = useId();
  const menu = (items?.length ?? 0) > 1;

  useEffect(() => {
    if (!open) {
      return;
    }
    const onPointer = (event: PointerEvent) => {
      const root = rootRef.current;
      if (root !== null && !root.contains(event.target as Node)) {
        setOpen(false);
      }
    };
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        setOpen(false);
      }
    };
    document.addEventListener("pointerdown", onPointer);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", onPointer);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const run = () => {
    if (menu) {
      setOpen((was) => !was);
      return;
    }
    if (onClick !== undefined) {
      onClick();
      return;
    }
    items?.[0]?.onSelect();
  };

  return (
    <div className="rail__addwrap" ref={rootRef}>
      <button
        type="button"
        className="rail__add"
        title={label}
        aria-label={label}
        aria-haspopup={menu ? "menu" : undefined}
        aria-expanded={menu ? open : undefined}
        aria-controls={menu && open ? menuId : undefined}
        disabled={disabled}
        onClick={run}
      >
        +
      </button>
      {menu && open && items !== undefined ? (
        <ul className="rail__menu" id={menuId} role="menu" aria-label={label}>
          {items.map((item) => (
            <li key={item.id} role="none">
              <button
                type="button"
                role="menuitem"
                className="rail__menuitem"
                onClick={() => {
                  setOpen(false);
                  item.onSelect();
                }}
              >
                <span className="rail__menuname">{item.label}</span>
                {item.hint === undefined || item.hint === "" ? null : (
                  <span className="rail__menuhint">{item.hint}</span>
                )}
              </button>
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}
