! FLAGS: --std=f2023
! CHECK: direct=temporary result 1
! CHECK: nested=[temporary result 2]
! CHECK: trimmed=temporary result 3
! CHECK: fixed-io=temporary result 4
! CHECK: alloc-io=temporary result 5
! CHECK: conditional=temporary result 7
! CHECK: borrowed=borrowed result
! CHECK: plain-owned=temporary result 6
! CHECK: plain-borrowed=borrowed result
! CHECK: component-owned=temporary result 7
! CHECK: component-borrowed=borrowed result
! CHECK: array=temporary result 1
! CHECK: shift=temporary result 2
! CHECK: class-star=ok
! CHECK: class-assign=ok
! CHECK: actual-calls=20
! IR_CHECK: call @afs_modproc_deferred_char_result_temp_dealloc_mod_make_text
! IR_CHECK: rt_call @__afs_deallocate
! IR_CHECK: call @afs_modproc_deferred_char_result_temp_dealloc_mod_make_text
! IR_CHECK: rt_call @__afs_deallocate
! IR_CHECK: call @afs_modproc_deferred_char_result_temp_dealloc_mod_make_text
! IR_CHECK: rt_call @__afs_deallocate
module deferred_char_result_temp_dealloc_mod
  implicit none

  integer :: calls = 0

  interface consume
    module procedure consume_specific
  end interface consume

  abstract interface
    integer function consume_interface(text) result(length)
      character(*), intent(in) :: text
    end function consume_interface
    function owned_result_interface(index) result(text)
      integer, intent(in) :: index
      character(:), allocatable :: text
    end function owned_result_interface
    function borrowed_result_interface() result(text)
      character(:), pointer :: text
    end function borrowed_result_interface
  end interface

  type :: callback_t
    procedure(consume_interface), pointer, nopass :: invoke => null()
  end type callback_t

  type :: result_callback_t
    procedure(owned_result_interface), pointer, nopass :: owned => null()
    procedure(borrowed_result_interface), pointer, nopass :: borrowed => null()
  end type result_callback_t

  type :: holder_t
    class(*), allocatable :: value
  end type holder_t

  type :: consumer_t
  contains
    procedure :: accept
    procedure :: measure
  end type consumer_t

contains
  function make_text(index) result(text)
    integer, intent(in) :: index
    character(:), allocatable :: text
    character(1) :: digit

    calls = calls + 1
    write(digit, '(i1)') index
    text = 'temporary result ' // digit
  end function make_text

  function make_borrowed() result(text)
    character(:), pointer :: text
    character(15), target, save :: storage = 'borrowed result'

    text => storage
  end function make_borrowed

  subroutine consume_direct(text)
    character(*), intent(in) :: text

    if (len_trim(text) == 0) error stop 11
  end subroutine consume_direct

  subroutine consume_specific(text)
    character(*), intent(in) :: text

    if (len_trim(text) == 0) error stop 12
  end subroutine consume_specific

  subroutine consume_class_star(value)
    class(*), intent(in) :: value
  end subroutine consume_class_star

  subroutine check_class_text(value, expected)
    class(*), intent(in) :: value
    character(*), intent(in) :: expected

    select type (value)
    type is (character(len=*))
      if (value /= expected) error stop 21
    class default
      error stop 22
    end select
  end subroutine check_class_text

  subroutine accept(self, text)
    class(consumer_t), intent(in) :: self
    character(*), intent(in) :: text

    if (len_trim(text) == 0) error stop 13
  end subroutine accept

  integer function consume_function(text) result(length)
    character(*), intent(in) :: text

    length = len(text)
  end function consume_function

  integer function measure(self, text) result(length)
    class(consumer_t), intent(in) :: self
    character(*), intent(in) :: text

    length = len(text)
  end function measure
end module deferred_char_result_temp_dealloc_mod

program deferred_char_result_temp_dealloc
  use deferred_char_result_temp_dealloc_mod, only: callback_t, calls, consume, &
       borrowed_result_interface, check_class_text, consume_class_star, consume_direct, &
       consume_function, consume_interface, consumer_t, holder_t, make_borrowed, make_text, &
       owned_result_interface, result_callback_t
  implicit none

  type(callback_t) :: callback
  class(*), allocatable :: class_sink
  character(:), allocatable :: dynamic_sink
  character(18) :: constructed(1), shifted(2)
  character(32) :: sink
  procedure(consume_interface), pointer :: consume_ptr
  procedure(borrowed_result_interface), pointer :: borrowed_result_ptr
  procedure(owned_result_interface), pointer :: owned_result_ptr
  integer :: length
  logical :: take_owned
  type(consumer_t) :: consumer
  type(holder_t) :: holder
  type(result_callback_t) :: result_callback

  consume_ptr => consume_function
  callback%invoke => consume_function
  owned_result_ptr => make_text
  borrowed_result_ptr => make_borrowed
  result_callback%owned => make_text
  result_callback%borrowed => make_borrowed

  sink = make_text(1)
  print '(a,a)', 'direct=', trim(sink)

  sink = '[' // make_text(2) // ']'
  print '(a,a)', 'nested=', trim(sink)

  sink = trim(make_text(3))
  print '(a,a)', 'trimmed=', trim(sink)

  write(sink, '(a)') make_text(4)
  if (trim(sink) /= 'temporary result 4') error stop 18
  print '(a,a)', 'fixed-io=', trim(sink)

  write(dynamic_sink, '(a)') make_text(5)
  if (.not. allocated(dynamic_sink)) error stop 19
  if (dynamic_sink /= 'temporary result 5') error stop 20
  print '(a,a)', 'alloc-io=', dynamic_sink

  call consume_direct(make_text(4))
  call consume(make_text(5))
  call consumer%accept(make_text(6))

  take_owned = .true.
  sink = (take_owned ? make_text(7) : make_text(8))
  print '(a,a)', 'conditional=', trim(sink)

  take_owned = .false.
  sink = (take_owned ? make_text(8) : make_borrowed())
  print '(a,a)', 'borrowed=', trim(sink)

  length = consume_ptr(make_text(8))
  if (length /= 18) error stop 14
  length = callback%invoke(make_text(9))
  if (length /= 18) error stop 15
  length = consumer%measure(make_text(4))
  if (length /= 18) error stop 16

  sink = owned_result_ptr(6)
  print '(a,a)', 'plain-owned=', trim(sink)
  sink = borrowed_result_ptr()
  print '(a,a)', 'plain-borrowed=', trim(sink)
  sink = result_callback%owned(7)
  print '(a,a)', 'component-owned=', trim(sink)
  sink = result_callback%borrowed()
  print '(a,a)', 'component-borrowed=', trim(sink)

  constructed = [character(18) :: make_text(1)]
  print '(a,a)', 'array=', trim(constructed(1))

  shifted = eoshift([character(18) :: 'left', 'right'], 1, boundary=make_text(2))
  print '(a,a)', 'shift=', trim(shifted(2))

  call consume_class_star(make_text(3))
  print '(a)', 'class-star=ok'

  class_sink = make_text(4)
  call check_class_text(class_sink, 'temporary result 4')
  holder%value = make_text(5)
  call check_class_text(holder%value, 'temporary result 5')
  call assign_host_class()
  print '(a)', 'class-assign=ok'

  if (calls /= 20) error stop 17
  print '(a,i0)', 'actual-calls=', calls
contains
  subroutine assign_host_class()
    class_sink = make_text(6)
    call check_class_text(class_sink, 'temporary result 6')
  end subroutine assign_host_class
end program deferred_char_result_temp_dealloc
