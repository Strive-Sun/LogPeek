import { useEffect, useRef, useState } from 'react';

export interface MenuItem {
  label: string;
  danger?: boolean;
  onClick: () => void;
}

interface Props {
  x: number;
  y: number;
  items: MenuItem[];
  onClose: () => void;
}

export function ContextMenu(props: Props) {
  const ref = useRef<HTMLDivElement>(null);
  // 初次渲染后根据菜单尺寸做边界修正,避免超出窗口
  const [pos, setPos] = useState({ x: props.x, y: props.y });

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const { width, height } = el.getBoundingClientRect();
    const x = Math.min(props.x, window.innerWidth - width - 6);
    const y = Math.min(props.y, window.innerHeight - height - 6);
    setPos({ x: Math.max(4, x), y: Math.max(4, y) });
  }, [props.x, props.y]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === 'Escape' && props.onClose();
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [props]);

  return (
    <>
      <div className="ctx-backdrop" onClick={props.onClose} onContextMenu={(e) => { e.preventDefault(); props.onClose(); }} />
      <div ref={ref} className="ctx-menu" style={{ left: pos.x, top: pos.y }}>
        {props.items.map((it, i) => (
          <div
            key={i}
            className={'ctx-item' + (it.danger ? ' danger' : '')}
            onClick={() => {
              it.onClick();
              props.onClose();
            }}
          >
            {it.label}
          </div>
        ))}
      </div>
    </>
  );
}
