use super::*;
use super::internal::*;
use std::iter::Rev;

pub struct SliceIter<'data, T: 'data + Sync> {
    slice: &'data [T]
}

impl<'data, T: Sync + 'data> IntoParallelIterator for &'data [T] {
    type Item = &'data T;
    type Iter = SliceIter<'data, T>;

    fn into_par_iter(self) -> Self::Iter {
        SliceIter { slice: self }
    }
}

impl<'data, T: Sync + 'data> IntoParallelIterator for &'data Vec<T> {
    type Item = &'data T;
    type Iter = SliceIter<'data, T>;

    fn into_par_iter(self) -> Self::Iter {
        SliceIter { slice: self }
    }
}

impl<'data, T: Sync + 'data> ToParallelChunks<'data> for [T] {
    type Item = T;
    type Iter = ChunksIter<'data, T>;

    fn par_chunks(&'data self, chunk_size: usize) -> Self::Iter {
        ChunksIter { chunk_size: chunk_size, slice: self }
    }
}

impl<'data, T: Sync + 'data> ParallelIterator for SliceIter<'data, T> {
    type Item = &'data T;

    fn drive_unindexed<C>(self, consumer: C) -> C::Result
        where C: UnindexedConsumer<Self::Item>
    {
        bridge(self, consumer)
    }
}

impl<'data, T: Sync + 'data> BoundedParallelIterator for SliceIter<'data, T> {
    fn upper_bound(&mut self) -> usize {
        ExactParallelIterator::len(self)
    }

    fn drive<C>(self, consumer: C) -> C::Result
        where C: Consumer<Self::Item>
    {
        bridge(self, consumer)
    }
}

impl<'data, T: Sync + 'data> ExactParallelIterator for SliceIter<'data, T> {
    fn len(&mut self) -> usize {
        self.slice.len()
    }
}

impl<'data, T: Sync + 'data> IndexedParallelIterator for SliceIter<'data, T> {
    fn with_producer<CB>(self, callback: CB) -> CB::Output
        where CB: ProducerCallback<Self::Item>
    {
        callback.callback(SliceProducer { slice: self.slice })
    }
}

pub struct ChunksIter<'data, T: 'data + Sync> {
    chunk_size: usize,
    slice: &'data [T],
}

impl<'data, T: Sync + 'data> ParallelIterator for ChunksIter<'data, T> {
    type Item = &'data [T];

    fn drive_unindexed<C>(self, consumer: C) -> C::Result
        where C: UnindexedConsumer<Self::Item>
    {
        bridge(self, consumer)
    }
}

impl<'data, T: Sync + 'data> BoundedParallelIterator for ChunksIter<'data, T> {
    fn upper_bound(&mut self) -> usize {
        ExactParallelIterator::len(self)
    }

    fn drive<C>(self, consumer: C) -> C::Result
        where C: Consumer<Self::Item>
    {
        bridge(self, consumer)
    }
}

impl<'data, T: Sync + 'data> ExactParallelIterator for ChunksIter<'data, T> {
    fn len(&mut self) -> usize {
        (self.slice.len() + (self.chunk_size - 1)) / self.chunk_size
    }
}

impl<'data, T: Sync + 'data> IndexedParallelIterator for ChunksIter<'data, T> {
    fn with_producer<CB>(self, callback: CB) -> CB::Output
        where CB: ProducerCallback<Self::Item>
    {
        callback.callback(SliceChunksProducer { chunk_size: self.chunk_size, slice: self.slice })
    }
}

///////////////////////////////////////////////////////////////////////////

pub struct SliceProducer<'data, T: 'data + Sync> {
    slice: &'data [T]
}

pub struct SliceRevProducer<'data, T: 'data + Sync> {
    slice: &'data [T]
}

impl<'data, T: 'data + Sync> Producer for SliceProducer<'data, T> {
    type DoubleEndedIterator = ::std::slice::Iter<'data, T>;
    type RevProducer = SliceRevProducer<'data, T>;

    fn cost(&mut self, len: usize) -> f64 {
        len as f64
    }

    fn split_at(self, index: usize) -> (Self, Self) {
        let (left, right) = self.slice.split_at(index);
        (SliceProducer { slice: left }, SliceProducer { slice: right })
    }

    fn rev(self) -> Self::RevProducer {
       SliceRevProducer {
           slice: self.slice
       }
    }
}

impl<'data, T: 'data + Sync> Producer for SliceRevProducer<'data, T> {
    type DoubleEndedIterator = ::std::slice::Iter<'data, T>;
    type RevProducer = SliceProducer<'data, T>;

    fn cost(&mut self, len: usize) -> f64 {
        len as f64
    }

    fn split_at(self, index: usize) -> (Self, Self) {
        //FIXME FIXME FIXME - this probably needs to be updated
        let (left, right) = self.slice.split_at(index);
        (SliceRevProducer { slice: left }, SliceRevProducer { slice: right })
    }

    fn rev(self) -> Self::RevProducer {
       SliceProducer {
           slice: self.slice
       }
    }
}

impl<'data, T: 'data + Sync> IntoIterator for SliceProducer<'data, T> {
    type Item = &'data T;
    type IntoIter = ::std::slice::Iter<'data, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.slice.into_iter()
    }
}

impl<'data, T: 'data + Sync> IntoIterator for SliceRevProducer<'data, T> {
    type Item = &'data T;
    type IntoIter = Rev<::std::slice::Iter<'data, T>>;

    fn into_iter(self) -> Self::IntoIter {
        self.slice.into_iter().rev()
    }
}

pub struct SliceChunksProducer<'data, T: 'data + Sync> {
    chunk_size: usize,
    slice: &'data [T]
}

pub struct SliceChunksRevProducer<'data, T: 'data + Sync> {
    chunk_size: usize,
    slice: &'data [T]
}

impl<'data, T: 'data + Sync> Producer for SliceChunksProducer<'data, T> {
    type DoubleEndedIterator = ::std::slice::Chunks<'data, T>;
    type RevProducer = SliceChunksRevProducer<'data, T>;

    fn cost(&mut self, len: usize) -> f64 {
        len as f64
    }

    fn split_at(self, index: usize) -> (Self, Self) {
        let elem_index = index * self.chunk_size;
        let (left, right) = self.slice.split_at(elem_index);
        (SliceChunksProducer { chunk_size: self.chunk_size, slice: left },
         SliceChunksProducer { chunk_size: self.chunk_size, slice: right })
    }

    fn rev(self) -> Self::RevProducer {
        SliceChunksRevProducer {
            chunk_size: self.chunk_size,
            slice: self.slice
        }
    }
}

impl<'data, T: 'data + Sync> Producer for SliceChunksRevProducer<'data, T> {
    type DoubleEndedIterator = ::std::slice::Chunks<'data, T>;
    type RevProducer = SliceChunksProducer<'data, T>;

    fn cost(&mut self, len: usize) -> f64 {
        len as f64
    }

    fn split_at(self, index: usize) -> (Self, Self) {
        //FIXME FIXME FIXME - this probably needs to be updated
        let elem_index = index * self.chunk_size;
        let (left, right) = self.slice.split_at(elem_index);
        (SliceChunksRevProducer { chunk_size: self.chunk_size, slice: left },
         SliceChunksRevProducer { chunk_size: self.chunk_size, slice: right })
    }

    fn rev(self) -> Self::RevProducer {
        SliceChunksProducer {
            chunk_size: self.chunk_size,
            slice: self.slice
        }
    }
}

impl<'data, T: 'data + Sync> IntoIterator for SliceChunksProducer<'data, T> {
    type Item = &'data [T];
    type IntoIter = ::std::slice::Chunks<'data, T>;

    fn into_iter(self) -> Self::IntoIter {
        self.slice.chunks(self.chunk_size)
    }
}

impl<'data, T: 'data + Sync> IntoIterator for SliceChunksRevProducer<'data, T> {
    type Item = &'data [T];
    type IntoIter = Rev<::std::slice::Chunks<'data, T>>;

    fn into_iter(self) -> Self::IntoIter {
        self.slice.chunks(self.chunk_size).rev()
    }
}
