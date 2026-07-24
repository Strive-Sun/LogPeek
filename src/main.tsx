import React, { useEffect } from 'react';
import ReactDOM from 'react-dom/client';
import { App } from './App';
import { I18nProvider } from './i18n/I18nProvider';
import { beginStartupHandoff } from './startup';
import './styles.css';

function StartupLifecycle() {
  useEffect(() => beginStartupHandoff(), []);
  return null;
}

ReactDOM.createRoot(document.getElementById('root')!).render(
  <React.StrictMode>
    <I18nProvider>
      <StartupLifecycle />
      <App />
    </I18nProvider>
  </React.StrictMode>,
);
