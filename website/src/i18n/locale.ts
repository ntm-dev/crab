import { createContext, useContext } from 'react';
import { en, type Messages } from './en';
import { vi } from './vi';

export type Locale = 'en' | 'vi';

export const locales: Locale[] = ['en', 'vi'];

const messagesByLocale: Record<Locale, Messages> = { en, vi };

const pathPrefixes: Record<Locale, string> = { en: '', vi: '/vi' };

export function isLocale(value: string): value is Locale {
  return (locales as string[]).includes(value);
}

export function messagesFor(locale: Locale): Messages {
  return messagesByLocale[locale];
}

export function localePath(locale: Locale, path: string): string {
  return `${pathPrefixes[locale]}${path}`;
}

export interface PageInfo {
  locale: Locale;
  path: string;
}

export const PageContext = createContext<PageInfo>({ locale: 'en', path: '/' });

export function usePage(): PageInfo {
  return useContext(PageContext);
}

export function useMessages(): Messages {
  return messagesFor(usePage().locale);
}
