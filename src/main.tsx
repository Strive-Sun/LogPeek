import React from 'react';
import ReactDOM from 'react-dom/client';
import { App } from './App';
import { I18nProvider } from './i18n/I18nProvider';
import './styles.css';

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <I18nProvider>
      <App />
    </I18nProvider>
  </React.StrictMode>,
);

// React 已挂载:等浏览器绘制一帧应用界面后,让启动加载页冲到 100% 并淡出,
// 过渡结束(或超时兜底)后移除节点。加载页位于 #root 外,不受 React 清空影响。
const bootLoader = document.getElementById('boot-loader');
if (bootLoader) {
  requestAnimationFrame(() =>
    requestAnimationFrame(() => {
      bootLoader.classList.add('done');
      let removed = false;
      const remove = () => {
        if (removed) return;
        removed = true;
        bootLoader.remove();
      };
      const handleTransitionEnd = (event: TransitionEvent) => {
        if (event.target === bootLoader && event.propertyName === 'opacity') remove();
      };
      bootLoader.addEventListener('transitionend', handleTransitionEnd);
      // 兜底:transitionend 未触发(如动画被系统禁用)时仍移除
      setTimeout(remove, 600);
    }),
  );
}
