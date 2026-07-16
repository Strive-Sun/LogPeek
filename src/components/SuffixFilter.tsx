import { useState } from 'react';

interface Props {
  filter: string[];
  showAll: boolean;
  onFilterChange: (f: string[]) => void;
  onShowAllChange: (v: boolean) => void;
}

const SUFFIX_CHOICES = ['.log', '.txt', '.out', '.json'];

export function SuffixFilter(props: Props) {
  const [open, setOpen] = useState(false);
  const [custom, setCustom] = useState('');

  const toggleSuffix = (s: string) => {
    props.onFilterChange(
      props.filter.includes(s) ? props.filter.filter((x) => x !== s) : [...props.filter, s],
    );
  };

  const addCustom = () => {
    let s = custom.trim();
    if (!s) return;
    if (!s.startsWith('.')) s = '.' + s;
    if (!props.filter.includes(s)) props.onFilterChange([...props.filter, s]);
    setCustom('');
  };

  return (
    <div className="suffix-filter">
      <button className="suffix-btn" onClick={() => setOpen((v) => !v)} title="后缀筛选">
        后缀 ▾
      </button>
      {open && (
        <>
          <div className="backdrop" onClick={() => setOpen(false)} />
          <div className="pop suffix-pop">
            <div className="pop-head">后缀筛选</div>
            {SUFFIX_CHOICES.map((s) => (
              <div className="filter-row" key={s}>
                <label>
                  <input
                    type="checkbox"
                    checked={props.filter.includes(s)}
                    onChange={() => toggleSuffix(s)}
                  />
                  {s}
                </label>
              </div>
            ))}
            <div className="filter-row">
              <label>
                <input
                  type="checkbox"
                  checked={props.showAll}
                  onChange={(e) => props.onShowAllChange(e.target.checked)}
                />
                显示全部(含非日志)
              </label>
            </div>
            <div className="filter-custom">
              <input
                placeholder=".trace"
                value={custom}
                onChange={(e) => setCustom(e.target.value)}
                onKeyDown={(e) => e.key === 'Enter' && addCustom()}
              />
              <button className="icon-btn" onClick={addCustom}>+</button>
            </div>
          </div>
        </>
      )}
    </div>
  );
}
