use std::fmt::Debug;

use bimap::BiHashMap;

pub(crate) struct LRUCache<T> {
    cache: Vec<(usize, T)>,
    index_map: BiHashMap<u64, usize>,
    current_time: usize,
    // calc_size: fn(&T) -> usize,
    // size_sum: usize,
    // size_sum_limit: usize,
}

impl<T> LRUCache<T> {
    pub(crate) fn new() -> Self {
        Self {
            cache: vec![],
            index_map: Default::default(),
            current_time: 0,
        }
    }

    pub(crate) fn contains(&self, index: u64) -> bool {
        self.index_map.contains_left(&index)
    }

    pub(crate) fn get_cache(&mut self, index: u64) -> Option<&T> {
        self.get_cache_mut(index).map(|v| v as &T)
    }

    pub(crate) fn get_cache_mut(&mut self, index: u64) -> Option<&mut T> {
        if let Some(i) = self.index_map.get_by_left(&index).copied() {
            let mut value = self.cache.swap_remove(i);
            self.current_time += 1;
            value.0 = self.current_time;
            self.index_map.insert(*self.index_map.get_by_right(&self.cache.len()).unwrap(), i);
            let mut current = i;
            while current * 2 + 1 < self.cache.len() {
                if self.cache[current].0 < self.cache[current * 2 + 1].0 &&
                    (current * 2 + 2 >= self.cache.len() || self.cache[current].0 < self.cache[current * 2 + 2].0) {
                    break;
                } else if current * 2 + 2 >= self.cache.len() || self.cache[current * 2 + 1].0 < self.cache[current * 2 + 2].0 {
                    let a = *self.index_map.get_by_right(&current).unwrap();
                    let b = *self.index_map.get_by_right(&(current * 2 + 1)).unwrap();
                    self.index_map.insert(b, current);
                    self.index_map.insert(a, current * 2 + 1);
                    self.cache.swap(current, current * 2 + 1);
                    current = current * 2 + 1;
                } else {
                    let a = *self.index_map.get_by_right(&current).unwrap();
                    let b = *self.index_map.get_by_right(&(current * 2 + 2)).unwrap();
                    self.index_map.insert(b, current);
                    self.index_map.insert(a, current * 2 + 2);
                    self.cache.swap(current, current * 2 + 2);
                    current = current * 2 + 2;
                }
            }
            self.index_map.insert(index, self.cache.len());
            self.cache.push(value);
            self.cache.last_mut().map(|v| &mut v.1)
        } else {
            None
        }
    }

    pub(crate) fn insert_cache(&mut self, index: u64, value: T) -> &T {
        if self.contains(index) {
            let ptr = self.get_cache_mut(index).unwrap();
            *ptr = value;
            ptr
        } else {
            self.current_time += 1;
            self.index_map.insert(index, self.cache.len());
            self.cache.push((self.current_time, value));
            //TODO:古いの削除する
            &self.cache.last().unwrap().1
        }
    }
}

mod test {
    use crate::github_filesystem::cache::LRUCache;

    #[test]
    fn test1() {
        let mut cache = LRUCache::new();
        for i in 0..8 {
            cache.insert_cache(i as u64, i);
            println!("{:?}", cache.cache);
            println!("{:?}", cache.index_map);
        }
        println!("----------------------------------------------");
        for i in 0..8 {
            assert_eq!(cache.get_cache(i as u64), Some(&i));
            println!("{:?}", cache.cache);
            println!("{:?}", cache.index_map);
        }
    }
}
