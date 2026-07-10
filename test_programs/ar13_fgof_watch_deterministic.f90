! Real-world-shaped O2/Os determinism regression from watch-entry filtering.
!
! REPRO_CHECK: asm
! CHECK: 1
! CHECK: root/src
module ar13_watch_types
  use, intrinsic :: iso_fortran_env, only : int64
  implicit none

  type :: watch_entry
    character(len=:), allocatable :: path
    integer(int64) :: inode = 0_int64
    integer(int64) :: size = 0_int64
    integer(int64) :: mtime_sec = 0_int64
    integer(int64) :: mtime_nsec = 0_int64
    logical :: is_directory = .false.
  end type watch_entry

  type :: watch_options
    logical :: ignore_hidden = .false.
    character(len=:), allocatable :: ignore_prefixes(:)
    integer, allocatable :: ignore_prefix_lengths(:)
  end type watch_options
end module ar13_watch_types

module ar13_watch_filter
  use ar13_watch_types, only : watch_entry, watch_options
  implicit none
contains
  subroutine filter_entries(root, options, entries)
    character(len=*), intent(in) :: root
    type(watch_options), intent(in) :: options
    type(watch_entry), allocatable, intent(inout) :: entries(:)
    type(watch_entry), allocatable :: filtered(:)
    integer :: i

    allocate(filtered(0))
    do i = 1, size(entries)
      if (entry_is_ignored(root, options, entries(i))) cycle
      call append_entry(filtered, entries(i))
    end do
    call move_alloc(filtered, entries)
  end subroutine filter_entries

  logical function entry_is_ignored(root, options, entry) result(ignored)
    character(len=*), intent(in) :: root
    type(watch_options), intent(in) :: options
    type(watch_entry), intent(in) :: entry

    ignored = .false.

    if (options%ignore_hidden) then
      if (contains_hidden_segment(path_after_root(root, entry%path))) then
        ignored = .true.
        return
      end if
    end if

    if (path_matches_ignore_prefix(options, entry%path)) then
      ignored = .true.
    end if
  end function entry_is_ignored

  logical function path_matches_ignore_prefix(options, path) result(matches)
    type(watch_options), intent(in) :: options
    character(len=*), intent(in) :: path
    integer :: i
    integer :: prefix_len

    matches = .false.
    if (.not. allocated(options%ignore_prefixes)) return

    do i = 1, size(options%ignore_prefixes)
      prefix_len = ignore_prefix_length(options, i)
      if (prefix_len == 0) cycle
      if (len(path) == prefix_len) then
        if (path == options%ignore_prefixes(i)(1:prefix_len)) then
          matches = .true.
          return
        end if
      end if
      if (len(path) > prefix_len) then
        if (path(1:prefix_len) == options%ignore_prefixes(i)(1:prefix_len) .and. path(prefix_len + 1:prefix_len + 1) == '/') then
          matches = .true.
          return
        end if
      end if
    end do
  end function path_matches_ignore_prefix

  integer function ignore_prefix_length(options, index_value) result(length_value)
    type(watch_options), intent(in) :: options
    integer, intent(in) :: index_value

    if (allocated(options%ignore_prefix_lengths)) then
      length_value = options%ignore_prefix_lengths(index_value)
    else
      length_value = len_trim(options%ignore_prefixes(index_value))
    end if
  end function ignore_prefix_length

  function path_after_root(root, path) result(relative)
    character(len=*), intent(in) :: root
    character(len=*), intent(in) :: path
    character(len=:), allocatable :: relative

    if (path == root) then
      relative = basename_text(root)
      return
    end if

    if (len(path) > len(root)) then
      if (path(1:len(root)) == root .and. path(len(root) + 1:len(root) + 1) == '/') then
        relative = path(len(root) + 2:)
        return
      end if
    end if

    relative = path
  end function path_after_root

  logical function contains_hidden_segment(path) result(has_hidden)
    character(len=*), intent(in) :: path
    integer :: i
    integer :: start
    integer :: n

    has_hidden = .false.
    n = len(path)
    if (n == 0) return

    start = 1
    do i = 1, n + 1
      if (i <= n .and. path(i:i) /= '/') cycle
      if (i > start) then
        if (path(start:start) == '.') then
          has_hidden = .true.
          return
        end if
      end if
      start = i + 1
    end do
  end function contains_hidden_segment

  function basename_text(path) result(name)
    character(len=*), intent(in) :: path
    character(len=:), allocatable :: name
    integer :: i

    do i = len(path), 1, -1
      if (path(i:i) == '/') then
        name = path(i + 1:)
        return
      end if
    end do

    name = path
  end function basename_text

  subroutine append_entry(entries, entry)
    type(watch_entry), allocatable, intent(inout) :: entries(:)
    type(watch_entry), intent(in) :: entry
    type(watch_entry), allocatable :: grown(:)
    integer :: n

    n = size(entries)
    allocate(grown(n + 1))
    if (n > 0) grown(1:n) = entries
    grown(n + 1) = entry
    call move_alloc(grown, entries)
  end subroutine append_entry
end module ar13_watch_filter

program ar13_fgof_watch_deterministic
  use ar13_watch_filter, only : filter_entries
  use ar13_watch_types, only : watch_entry, watch_options
  implicit none
  type(watch_options) :: options
  type(watch_entry), allocatable :: entries(:)

  allocate(entries(2))
  entries(1)%path = 'root/.cache'
  entries(2)%path = 'root/src'
  options%ignore_hidden = .true.
  call filter_entries('root', options, entries)

  print *, size(entries)
  print '(a)', entries(1)%path
end program ar13_fgof_watch_deterministic
