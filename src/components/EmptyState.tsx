interface Props {
  onAddDir: () => void;
}

export function EmptyState({ onAddDir }: Props) {
  return (
    <div className="col col-content">
      <div className="empty-state">
        <div className="big">📂</div>
        <div className="title">还没有监控目录</div>
        <div className="desc">添加一个目录,LogPeek 会自动发现新到达的日志压缩包和文本文件</div>
        <button className="cta" onClick={onAddDir}>
          + 添加监控目录
        </button>
      </div>
    </div>
  );
}
