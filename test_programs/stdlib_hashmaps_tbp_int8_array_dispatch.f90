! CHECK: ok
! IR_CHECK: afs_modproc_m_int8_map_entry
! REPRO_CHECK: run
module m
  use iso_fortran_env, only: int8, int32, int64
  implicit none

  type :: key_type
    integer(int8), allocatable :: value(:)
  end type key_type

  abstract interface
    function hasher_fun(key) result(hash_code)
      import :: int32, key_type
      type(key_type), intent(in) :: key
      integer(int32) :: hash_code
    end function hasher_fun

    subroutine key_map_entry_ifc(map, key)
      import :: hashmap_type, key_type
      class(hashmap_type), intent(inout) :: map
      type(key_type), intent(in) :: key
    end subroutine key_map_entry_ifc
  end interface

  type, abstract :: hashmap_type
    integer :: seen = 0
    procedure(hasher_fun), pointer, nopass :: hasher => sum_key
  contains
    procedure(key_map_entry_ifc), deferred, pass(map) :: key_map_entry
    procedure, non_overridable, pass(map) :: int8_map_entry
    generic, public :: map_entry => key_map_entry, int8_map_entry
  end type hashmap_type

  type, extends(hashmap_type) :: chaining_hashmap_type
  contains
    procedure :: key_map_entry => map_chain_entry
  end type chaining_hashmap_type
contains
  subroutine set_int8(key, value)
    type(key_type), intent(out) :: key
    integer(int8), intent(in) :: value(:)

    key%value = value
  end subroutine set_int8

  subroutine int8_map_entry(map, value)
    class(hashmap_type), intent(inout) :: map
    integer(int8), intent(in) :: value(:)
    type(key_type) :: key

    call set_int8(key, value)
    call map%key_map_entry(key)
  end subroutine int8_map_entry

  subroutine map_chain_entry(map, key)
    class(chaining_hashmap_type), intent(inout) :: map
    type(key_type), intent(in) :: key

    map%seen = size(key%value)
    if (map%hasher(key) < 0) error stop 10
  end subroutine map_chain_entry

  function sum_key(key) result(hash_code)
    type(key_type), intent(in) :: key
    integer(int32) :: hash_code

    hash_code = int8_sum(key%value)
  end function sum_key

  function int8_sum(key) result(hash_code)
    integer(int8), intent(in) :: key(:)
    integer(int32) :: hash_code
    integer(int64) :: i

    hash_code = 0_int32
    do i = 1_int64, size(key, kind=int64)
      hash_code = hash_code + int(key(i), int32)
    end do
  end function int8_sum
end module m

program p
  use iso_fortran_env, only: int8, int64
  use m, only: chaining_hashmap_type
  implicit none

  type(chaining_hashmap_type) :: map

  call map%map_entry([1_int8, 2_int8, 3_int8])
  if (map%seen /= 3) error stop 1

  call map%map_entry(transfer([1_int64, 2_int64, 3_int64], [0_int8]))
  if (map%seen /= 24) error stop 2

  print *, 'ok'
end program p
